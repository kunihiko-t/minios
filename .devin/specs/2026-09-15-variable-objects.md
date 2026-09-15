# 所有権台帳のheap化（可変個のkernel object管理）

## 背景

`AddressSpaceStorage<N>`は`[Option<OwnedFrame>; N]`の固定容量LIFO台帳で、
`AddressSpace`が`&'storage mut`で借用する。この形のために

- `AddressSpace`/`AddressSpaceBuilder`/`LoadedImage`/`Process`/`UserRun`/
  `ProcessTable`/各Failure型へ`'storage` lifetimeと`const N` genericが
  伝播している
- `MAX_OWNED_FRAMES = 2688`の静的arenaをboot時に4+個静的確保している
  （1個あたり約43 KiB、.bss計約170 KiB超）
- processごとの所有frame数がコンパイル時定数で頭打ちになる

heapがframe poolから動的に成長できるようになったので、台帳をheap-backedに
変え、これらの制限と配管をまとめて取り除く。

## 設計

- `AddressSpaceStorage`は`Vec<OwnedFrame>`を包むnewtypeへ変わる
  （`const N`削除）。`push`は`try_reserve(1)`で失敗可能を保ち、
  OOM時は`CapacityExceeded`+frameを返す既存契約を維持する。
- `AddressSpace`はstorageを`&mut`借用ではなく**所有**する。
  `'storage` lifetimeと`const N`を全関連型から除去する。
- `AddressSpaceBuilder`はstorageを内部で`new()`して`finish()`でspaceへ
  移す。`storage.is_empty()`前提と`VmError::StorageInUse`は消える。
- lib.rsへ`#[cfg(not(target_arch = "riscv32"))] extern crate alloc`を追加。
  RV32 lib buildは対象外のためRV32経路へ影響しない。
- main.rsの`KERNEL_/ELF_/USER_PROBE_ADDRESS_SPACE_STORAGE`と
  `PROCESS_STORAGES`静的arenaを削除。`AddressSpaceBuilder::new`と
  `load_image*`/`Process::spawn`/`UserRun::new`からstorage引数を除去。
- 回復後の`storage.is_empty()`/`len`診断は構造上不要になるため除去し、
  allocator統計の照合だけを残す。

## 不変条件の変化

- 旧: 「arenaが空であること」を`is_empty`で検査 → 新: ledgerはspaceごとに
  所有されるため、構造的に常に専用かつ初期化時に空。
- 旧: `MAX_OWNED_FRAMES`で1 address spaceの所有frame数を制限 → 新:
  上限はframe poolとheap成長で決まる。
- `destroy`/`reclaim`の「失敗時は所有権を戻す」契約は不変
  （popした要素の分だけcapacityが残るため`restore_last`は失敗しない）。

## 検証

- host test: fixtureの`AddressSpaceStorage::<N>`を除去し、
  失敗注入テスト（fail_zero_after等）と回収検査を維持する。
  旧容量依存テストは「旧上限を超える所有frame数」を検査する成長テストへ
  置き換える。
- `qemu-test-elf`/`user-exit`/`payload`/`sched`系は回収経路を
  end-to-endで網羅済み。
- `qemu-test-heap`は台帳がheapに常駐するため、統計検査をtest自身の
  割り当てに対する差分（baseline相対）へ変える。
- 全34 phaseゲート。

## 非目標

- `ProcessTable`自体の可変長化（`MAX_PROCS`固定は据え置き。台帳のheap化が
  その前提となる）。
- heap縮小・page返却。
