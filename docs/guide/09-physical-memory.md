# 物理ページ管理

MiniOS がここで扱うのは、MMU を有効にする前の物理アドレスです。仮想アドレスは将来 Sv39
ページテーブルで物理アドレスへ変換されますが、物理 page allocator はその変換先となる RAM
のページを渡します。

## 範囲と整列

1 ページは 4 KiB (`4096` byte) です。allocator は開始・終了・`PhysFrame` の開始アドレスが
4 KiB 整列している場合だけ受け入れます。QEMU virt の RAM は `0x8000_0000..0x8800_0000`
ですが、OpenSBI が置かれる低位側と、`__kernel_end` までのカーネル image は使用済みです。
従って初期化範囲は `align_up(__kernel_end, 4096)..0x8800_0000` です。

128 MiB は 32,768 ページなので、1 ページを 1 bit で表す bitmap は 32,768 bit、すなわち
4 KiB です。`FrameAllocator<512>` は `u64` 512 word = 32,768 bit を持ち、この QEMU RAM 全体を
表現できます。bitmap 自体はカーネル image 内にあるため、割り当て対象には入りません。

## 割り当てと解放

`allocate` は bitmap の先頭から最初の 0 bit を探す first-fit です。bit を 1 にして対応する
物理 frame を返すので、解放した低い番号のページは次の割り当てで再利用されます。空き bit が
なければ `None` を返します。

`deallocate` は allocator 範囲外を `OutOfRange`、すでに 0 bit のページを `DoubleFree` として
拒否します。この検査は二重解放によって同一物理ページを二つの利用者へ渡す事故を防ぎます。
`stats` は bitmap を走査せず、割り当て成功・解放成功ごとに保つ個数から total / allocated /
free を返します。

allocator は `kernel_main` が所有するローカル値です。global mutable state を置かないため、
初期化順序と可変借用の責任が呼び出し側に見え、将来 shell へ `&mut FrameAllocator<512>` を渡す
境界も明確になります。

## 演習

1. 小さな bitmap を使い、全ページを確保した後に `None` になることを観察してください。
2. 交互にページを解放してから再確保し、first-fit が低い穴を先に埋める断片化の様子を調べてください。
