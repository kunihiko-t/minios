# ProcessTable の heap-backed 化と安定 pid

2026-09-16

## 背景

`ProcessTable` は固定長配列 `slots: [Option<Process>; MAX_PROCS]` であり、
slot index が pid を兼ねる。fd所有権refactor（PR #40）で `Process` が
fd tableを内蔵し自己完結したため、table側の格納方式を変えられる状態に
なった。

現行方式の問題:

- slot index = pid なので、processが終了してslotを再利用するとpidも
  再利用される。将来processを実行中にspawnする機能（`fork`/`spawn`
  syscall）を付けると、PROC_EXIT frameが参照するpidと新processのpidが
  衝突する。
- tableは常に`MAX_PROCS`分の`Process`をstatic占有する。process数に
  比例したメモリーの方が所有権の教え方として自然。
- `Option<Process>`の二重構造は、実際には「live processの集合」を
  表すのにempty slot概念を持ち込んでいる。

## 目標

- `ProcessTable`の格納をheap-backed `Vec<Process>`へ変え、
  live process数に比例したメモリーにする。
- pidをslot indexから単調採番へ変え、process自身が`pid`を保持する。
  終了したpidは再利用しない（u64ラップは現実的に到達不能だが
  `checked_add`で明示する）。
- `insert`/`get`/`take`/`pick_next`/`wake_all_blocked`/
  `revoke_file_fds`/`len`/`is_empty`のAPI形を維持する。
- 既存の全syscall動作・sched phases・PROC_EXIT frameを無変更に保つ。

## 設計

### `Process`

- `pid: usize` fieldを追加。`spawn`は`usize::MAX`（未割当）で初期化し、
  `ProcessTable::insert`が採番する。
- `pub const fn pid(&self) -> usize`を追加。

### `ProcessTable`

```rust
pub struct ProcessTable {
    procs: Vec<Process>,
    next_pid: usize,
    last_picked: Option<usize>,
}
```

- `insert`: `procs.len() == MAX_PROCS`なら`Err(process)`（admission cap =
  manifest上限を維持。Errがprocessを返す回収契約も維持）。採番は
  `next_pid`の`checked_add`で行い、overflow時は`Err(process)`。
- `get`/`get_mut`: `procs.iter().find(|p| p.pid == pid)`。
- `take(pid)`: pidの位置を探して`Vec::remove`（順序保持のため
  `swap_remove`ではなく`remove`）。取り出した`Process`を返す。
- `take_oldest`: `procs.is_empty()`なら`None`、さもなくば
  `procs.remove(0)`。`reclaim_process_table`のdrain経路用。
- `pick_next`: `last_picked`のpidの位置+1から時計回りに走査し、
  最初のrunnable processのpidを返して`last_picked`を更新。
  位置indexではなくpidを保持するため、removeによる詰め直しで
  不公平や脱線が起きない。
- `wake_all_blocked`/`revoke_file_fds`/`len`/`is_empty`: `procs`を
  直接走査する。
- `iter()`: spawn一覧表示用に`procs.iter()`を公開する。

### `main.rs`

- `reclaim_process_table`: `0..MAX_PROCS`のtake走査をやめ、
  `take_oldest`でdrainする。
- spawn一覧: `for pid in 0..MAX_PROCS`を`table.iter()`へ変え、
  `process.pid()`/`process.name()`を印字する。
- spawn loopの`pid != index` assertは維持（初回採番は0始まりなので
  manifest順と一致する）。

### unsafe pointerとの関係

`CURRENT_PROC`/`PROC_TABLE_PTR`のraw pointerは、Vec再配置の影響を
受けないよう既存の契約で保護される:

- `CURRENT_PROC`は`__run_user`直前に`get_mut`から取得したpointerを
  入れ、`ReturnToKernel`直後にnullへ戻す。`take`/`insert`による
  `Process`の移動はdispatch外（reclaim経路・spawn経路）でだけ起き、
  その時点でpointerはnull。
- `PROC_TABLE_PTR`はVec bufferではなく`ProcessTable`自身を指し、
  `revoke_file_fds`が呼ばれるたびに`&mut`を取り直すため、bufferの
  移動を問題にしない。

## host test

既存testの調整:

- `table_cycles_over_live_slots`: 期待値はそのまま（`last_picked`方式で
  同一のround-robin順になる）。
- `table_rejects_overflow_and_returns_process`: admission capを
  そのまま検証。
- `reused_slot_starts_with_empty_fd_table`: pid再利用がなくなったため
  期待値を`pid == 1`へ変え、pid非再利用のassertを兼ねる。

追加test:

- pid単調採番: `take`後の`insert`が前pidと異なる新pidを返す。
- `take_oldest`がmanifest順の先頭から取り出す。
- `Process::pid()`がinsert後に採番値を返す。

## 検証

- `cargo fmt --all -- --check`
- `cargo test -p minios-kernel --lib`
- `cargo clippy`（lib・RV64 bin・RV32 bin、`-D warnings`）
- RV64/RV32 build、guest build
- `cargo xtask test`（sched/sched-io/sched-io-partial含む全QEMU経路）
- `cargo xtask check` 全40フェーズ

## 非目標

- `fork`/`spawn`/`getpid` syscallの追加。
- `MAX_PROCS` admission capの撤廃または引き上げ。
- multi-hart・preempt中のVec再配置対策（単一ハートのため不要）。
