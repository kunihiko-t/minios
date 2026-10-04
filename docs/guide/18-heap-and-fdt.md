# 18. Device Treeとヒープの成長

## 学習目標

OpenSBIが`a1`で渡すDTBを読み、RAM範囲、UART base、timebaseを`MachineSpec`として取り出す流れを説明できるようになります。
DTBのRAM範囲から、FDT予約、payload窓、frame allocatorの管理上端を導く計算を追います。
16 byte粒度のfirst-fit free-listヒープが、割り当て、解放、併合をどう行うかを確認します。
空きが尽きたときに`allocate_at`で隣接pageを取り込み、ヒープを下方へ成長させる経路を確認します。
address spaceの所有frame台帳が固定長配列からheap上の`Vec`へ移った理由を説明できるようになります。

## 背景

第16章までのkernelは、QEMU `virt`の`-m 128M`を前提にした物理addressを定数として持っていました。
RAMの上端、UARTのMMIO base、timerの周波数はmachineごとに異なり、定数のままでは別のRAM量で起動しただけで配置が崩れます。
OpenSBIは起動時に`a1`へFlattened Device Tree（FDT、そのbinaryをDTBと呼びます）の物理addressを渡すため、kernelはこれを読んでmachine記述を組み立てられます。

もう一つの課題は、4 KiBのpage単位より細かいkernel objectの置き場所です。
所有frameの台帳やprocess tableを固定長の配列で持つと、最悪の件数に合わせた`.bss`を常に確保することになります。
汎用ヒープを用意して`alloc` crateの`Vec`や`Box`を使えるようにすれば、件数はframe poolが許す限り可変になります。

ヒープの初期領域だけでは大きな要求に応えられないため、MiniOSのヒープは空きが尽きるとframe poolからpageを借りて成長します。
ただし借りたpageはpoolへ返しません。

## 実装

### DTBからMachineSpecを作る

[`kernel/src/machine.rs`](../../kernel/src/machine.rs)の`discover`は、`kernel_main`の最初に一度だけ呼ばれます。
`dtb`が0でなく8 byte整列であることを確かめ、先頭40 byteのheaderから`totalsize`を読みます。
`totalsize`が`FDT_RESERVED_LEN`（2 MiB）を超える場合は`Truncated`で拒否し、その長さのsliceを[`MachineSpec::from_dtb`](../../kernel/src/fdt.rs)へ渡します。
解析後、DTBの番地が`fdt_region()`の内側にあることを確かめ、外にあれば`UnsupportedMachine`で拒否します。

`MACHINE` staticの既定値はQEMU `virt`の参照値です。
DTBの解析に失敗した場合もpanic診断は既定のUART baseから出力でき、沈黙した停止になりません。

[`kernel/src/fdt.rs`](../../kernel/src/fdt.rs)のparserは`&[u8]`だけを受け取る純粋なロジックであり、host testで検証します。
headerではmagic `0xd00d_feed`、`totalsize`、version 16以上を検査し、structure blockとstrings blockの範囲がblob内に収まることを確かめます。
`walk_structure`は`FDT_BEGIN_NODE`、`FDT_PROP`、`FDT_END_NODE`、`FDT_NOP`、`FDT_END`のtoken列を走査し、それ以外のtokenは`BadStructure`にします。
nodeの深さは`MAX_DEPTH`（8）、名前の長さは`MAX_NAME`（64）までに制限します。

`reg` propertyは、そのnode自身ではなく親nodeの`#address-cells`と`#size-cells`で解読します。
そのため`FDT_BEGIN_NODE`の時点で親のcellsを`NodeScan::reg_cells`へ写し、`decode_reg`はcells数0、1、2だけを受け付けます。
各nodeの評価は`FDT_END_NODE`で行い、名前が`memory`で始まり`device_type`が`memory`の最初のnodeをRAMとします。
UARTは`compatible`に`ns16550a`か`ns16550`を含む最初のnode、timebaseは`cpus` nodeの`timebase-frequency`です。
`virtio,mmio`互換のnodeは発見順に`VIRTIO_MMIO_MAX`（8）個まで記録します。

`from_dtb`は解析結果をさらに検査します。
RAMの両端が4096 byte整列であり、長さが`FDT_RESERVED_LEN`と`BUNDLE_MAX_LEN`（6 MiB）の和より大きく、`uart_base`が0でない場合だけ受理します。
RAMの末尾から順に、`fdt_region()`が最後の2 MiB、`payload_window()`がその直下の`BUNDLE_MAX_LEN`、`managed_end()`が窓の下端です。
`-m 128M`ではそれぞれ`0x87e0_0000..0x8800_0000`、`0x8780_0000..0x87e0_0000`、`0x8780_0000`になり、第16章の定数と一致します。

### first-fit free-listヒープ

[`kernel/src/memory/heap.rs`](../../kernel/src/memory/heap.rs)の`Heap`は、連続した`[start, end)`を16 byte（`GRANULE`）単位で分割します。
`kernel_main`は`managed_end()`から`KERNEL_HEAP_LEN`（1 MiB）を切り出して`Heap::init`へ渡し、`-m 128M`では`0x8770_0000..0x8780_0000`が初期領域になります。
frame allocatorの管理上端はこのヒープ初期位置まで下がり、二つの所有範囲は重なりません。

空きブロックは、先頭に`FreeNode`（大きさと次nodeへのpointer）を埋め込んだaddress昇順の単方向listです。
`alloc`はlistを先頭からたどり、要求を収める最初のブロックを選ぶfirst-fitです。
各割り当ては返すpointerの直前に16 byteの`Span` headerを置き、その割り当てが占有する領域全体の先頭と長さを記録します。
`carve`はspanを常にブロック先頭から切り出すため、alignmentで生じた先頭の端数はspan内のpaddingになります。
末尾の残りが16 byte未満ならspanへ吸収し、16 byte以上なら新しい空きブロックとしてlistへ残します。

`dealloc`はpointerの直前から`Span`を読み戻し、spanが領域内で粒度にそろっているかを検査して、違反を`InvalidPointer`で拒否します。
`insert_free`はaddress順の挿入位置を探す途中で既存の空きブロックとの重なりを検出し、`DoubleFree`で拒否します。
挿入時は直前のブロックと連続していれば併合し、`coalesce_forward`で直後のブロックとも続けて併合します。

### allocate_atによる下方への成長

[`kernel/src/memory/frame.rs`](../../kernel/src/memory/frame.rs)の`FrameAllocator::allocate_at`は、指定番地のframeだけを占有します。
未整列、管理範囲外、占有済みのいずれかでは`None`を返し、bitmapを変えません。
通常の`allocate`は低位から空きを探すため、低位からのprocess frameと上端からのヒープ成長は、poolの両端から中央へ向かって使われます。
`FrameSource` traitにも`allocate_at`があり、`kernel/src/main.rs`の`GlobalFrames`はlock付きの`GLOBAL_FRAMES`へ委譲します。

`Heap::extend_down`は`new_start + len`が現在の`start`と一致する領域だけを受け付け、一致しなければ`NotContiguous`を返してheapを変えません。
新領域を`insert_free`で空きlistへ繋いでから`start`を下げ、成長分はlive allocationではないため`allocated`は変えません。

[`kernel/src/main.rs`](../../kernel/src/main.rs)の`GlobalAlloc`実装は、`heap.alloc`が失敗すると`heap.start() - PAGE_SIZE`を`allocate_at`で要求し、`extend_down`で取り込んでから再び`alloc`を試します。
要求が収まるまでこれを1 pageずつ繰り返し、隣接pageが取れなければnullを返して`alloc` crateのOOM処理へ渡します。
`dealloc`は空きlistへ戻すだけで、取り込んだpageをframe poolへ返す経路はありません。
返却を省いたことで、ヒープは常に一つの連続領域であり続けます。

起動時の`frames_base`は、`FrameAllocator::<512>::CAPACITY_FRAMES`を超えるRAMの下端側を管理対象から外し、管理範囲をヒープの直下へ寄せます。
これにより、ヒープが`allocate_at`で要求する番地は常にbitmapの管理範囲内に届きます。

### 所有frame台帳のVec化

[`kernel/src/vm/table.rs`](../../kernel/src/vm/table.rs)の`AddressSpaceStorage`は、以前は`MAX_OWNED_FRAMES`件の固定長配列をprocessごとのstatic arenaとして借用していました。
現在は`Vec<OwnedFrame>`を自身で所有し、件数の上限はframe poolとヒープの成長だけで決まります。
`push`は`try_reserve(1)`で先に容量を確保し、失敗したら`CapacityExceeded`とframeを呼び出し側へ返すため、rollback経路はframeを失いません。

## 実行と確認

DTBから発見したmachine記述は次のコマンドで確認します。

```console
$ cargo xtask test fdt
...
Domain0 Next Arg1           : 0x0000000087e00000
...
[ok] memory
[MINIOS_TEST] fdt: ram=0x80000000..0x88000000 uart=0x10000000 timebase=10000000
[MINIOS_TEST] fdt: ok
phase 1/1 passed (elapsed: ...)
summary: PASSED all 1 phases (elapsed: ...)
```

host harnessは`ram=`、`uart=`、`timebase=`の値を含む行が完全一致することを検査します。
OpenSBIの`Next Arg1`がkernelへ渡すDTBの番地であり、`fdt_region()`の先頭`0x87e0_0000`と一致します。

ヒープの割り当てと成長は次のコマンドで確認します。

```console
$ cargo xtask test heap
...
[ok] memory
[MINIOS_TEST] heap: grew total=2101248 pool_free=29525
[MINIOS_TEST] heap: total=1048576 largest_free=1046512 blocks=2
[MINIOS_TEST] heap: ok
phase 1/1 passed (elapsed: ...)
summary: PASSED all 1 phases (elapsed: ...)
```

kernel側の`run_heap_test`は、`Vec`の成長と`Box`の再利用を確かめた後、`KERNEL_HEAP_LEN + PAGE_SIZE`の`try_reserve_exact`で初期領域を超える割り当てを要求します。
heapの`total`とframe poolの`allocated`がともに増え、解放後にheap側の`allocated`が元へ戻った場合だけ`heap: ok`を出力します。
frame poolの`allocated`は、取り込んだpageを返さないため減りません。
host harnessが検査するのは`heap: ok`の行で、`grew`と`total`の行は値を読むための診断です。

## よくある失敗

- `reg`を自nodeの`#address-cells`で解読する：cellsは子へ与える値であり、`reg`は親のcellsで読まないとaddressとsizeがずれます。
- `/cpus`の`#size-cells = <0>`を不正値として扱う：`cpu@n`の`reg`はsize fieldを持たず、`decode_reg`はcells数0を正当な形として受け付けます。
- DTBの置き場所を確かめない：FDTがRAM最後の2 MiBの外にあるmachineではpayload窓がDTBを上書きし得るため、`discover`は`UnsupportedMachine`で拒否します。
- ヒープ領域と離れたframeを`extend_down`へ渡す：`new_start + len`が`start`と一致しない領域は`NotContiguous`で拒否され、heapは成長しません。
- 解放したヒープpageがpoolへ戻ると期待する：取り込んだpageは空きlistに残るだけで、`FrameStats`の`allocated`は減りません。
- 所有frame台帳の`push`失敗でframeを捨てる：`CapacityExceeded`と一緒に返るframeは呼び出し側が`deallocate`しないとリークします。

## 演習

[`kernel/src/memory/heap.rs`](../../kernel/src/memory/heap.rs)のhost testへ、三つの割り当てを作り、中央、先頭、末尾の順に解放するtestを追加してください。
各解放後に`stats().free_blocks`を確かめ、最後に1へ戻ることで前後の併合が働いていることを確認します。

[`kernel/src/fdt.rs`](../../kernel/src/fdt.rs)のtest用DTBへ`serial`のnodeを二つ置き、`uart_base`が先に現れたnodeのaddressになることを確かめるtestを書いてください。
次に、`memory` nodeの`device_type`を外したDTBで`MissingMemory`になることを確かめます。

`run_heap_test`の`try_reserve_exact`の要求量を変えて`cargo xtask test heap`を実行し、`grew total=`の値がpage単位でどう増えるかを観察してください。
増分と要求量の差が、`Span` headerと初期領域の空きブロックの配置からどう説明できるかを考えます。

## 次の章

[第19章「プリエンプティブscheduler」](19-scheduler.md)では、heap上の`Vec`に置いたprocess tableを使い、timer割り込みで複数のprocessを切り替える仕組みを追います。
章の全体像は[学習ガイド](README.md)と[ロードマップ](../reference/roadmap.md)を参照してください。
