# 汎用heapの動的拡張（frame allocatorからの成長）

## 背景

heapはmanaged RAM末尾に固定1 MiBを切り出す単一連続領域のfirst-fit free-list
である（`kernel/src/memory/heap.rs`）。`FrameAllocator<512>`は
`managed_memory_start..heap_start`を下位から順に割り当てる。
`GlobalAlloc::alloc`がOOMを返すとそのまま失敗となり、heapが余っていても
process frameが尽きればその逆も同様に、分割比はboot時に固定される。

## 目標

- heapのOOM時にframe allocatorからページを取得してheapを成長させる。
- heapとprocess frameが同一poolを動的に分け合う（固定分割をやめる）。
- heapは常に単一連続領域 `[start, end)` を保ち、既存のspan検証・
  free-list併合・`dealloc`の範囲検査を変えない。

## 設計

### 連続性を保つ成長

heapは`managed_end`側の上端固定・下端を下げる方向にのみ成長する。
成長要求は常に「現在の`heap.start`直下の1 page」なので、
`FrameAllocator::allocate_at(start)`（指定frameが空きなら占有する新API）で
`heap.start - PAGE_SIZE`だけを取得する。process向け`allocate`がbottom-up
first-fitであるためpool上端はheap成長だけが消費し、隣接性は自然に保たれる。
隣接frameが占有済み（≒pool枯渇）なら成長は失敗し、allocはOOMを返す。
成長したpageはheapへ返さない（縮小は行わない）。

### `Heap`側の変更

- `extend_down(new_start, len)`: `new_start + len == self.start`を検査し、
  `[new_start, old_start)`をfree blockとして挿入（先頭方向なのでhead側へ
  繋がる）。donated blockはlive allocationでないため`allocated`を減らさない
  —`insert_free`のaccountingを分離し、dealloc経路だけが減算する。
- `start()` getterを追加（KernelHeapが隣接frame番地を求めるため）。
- `HeapError::NotContiguous`を追加。

### `FrameSource` traitとallocatorの大域化

`GlobalAlloc::alloc`からframeを引くため、frame allocatorをstatic化する。
`main.rs`に`LOCKED_FRAMES`（spinlock + `UnsafeCell<FrameAllocator<512>>`）と
それを包む`GlobalFrames` ZSTを置く。

lib側の`&mut FrameAllocator<WORDS>`引数は`&mut dyn FrameSource`へ置き換える。
trait面: `allocate` / `deallocate` / `deallocate_recoverable` /
`allocator_id` / `stats`（現行利用メソッドの全量）。
`FrameAllocator`自体も`FrameSource`を実装するため、既存のホストテストは
具象allocatorをそのまま渡せる。

lock規則: heap lockを保持したまま`GlobalFrames`経由でframe lockを取る
（heap→frames一方向）。`GlobalFrames`の各メソッドは臨界区間がbitmap操作のみで
heapを割り当てないため逆順序は存在しない。割り込みhandler内で割り当てない
既存規約も不変。

### `KernelHeap::alloc`の成長経路

```
heap.alloc(layout) → Err(OutOfMemory) のとき:
    loop:
        frame = GLOBAL_FRAMES.allocate_at(heap.start() - PAGE_SIZE)
        失敗 → null (OOM)
        heap.extend_down(frame.start(), PAGE_SIZE)
        heap.alloc(layout) → Ok なら返す、Err なら次のpageへ
```

隣接frameが取れなければ即OOM。成長成功のたびに再試行し、要求が収まるまで
pageを積む（上限はpool残量で自然に打ち止まる）。

## 検証

- `frame.rs` host test: `allocate_at`の成功/範囲外/未整列/占有済み拒否。
- `heap.rs` host test: `extend_down`で領域が下方へ伸び`stats.total`が増え、
  新領域からの割り当て・解放・併合が働く。非隣接`extend_down`は拒否。
- 既存host test全維持（trait化で`&mut FrameAllocator`がtrait objectへ
  変わるだけでロジック不変）。
- `qemu-test-heap`: 1 MiB超のalloc列で`stats.total`の増加と
  frame pool残量の減少を報告する検査を追加。

## 非目標

- heapの縮小（free pageをframe poolへ返す）—将来の課題。
- 不連続extentの管理（単一連続領域の仮定を維持する）。
- 初期heap領域`KERNEL_HEAP_LEN`の縮小—成長経路の検証後に別途検討。
