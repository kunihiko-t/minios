# fd tableのprocess所有化

## 目的

`FILE_FDS[pid][slot]`は`Process`の外にあるstatic配列であり、fdの寿命と
processの寿命がコード上独立している。回収時の`close_all_file_fds`で
明示的に掃除する設計は、fdをprocess所有へ移せば**構造的に不要**になる。
`CURRENT_PID` indexも`pid=slot index`の結合を強いるため、実行中processへの
pointerへ置き換える。動的`ProcessTable`への布石でもある。

## 設計

- `FileFd`（`desc`/`offset`/`writable`）を`main.rs`から`process.rs`へ
  移し、fieldをprivate+accessor化する（`FileDesc`と同じ規約）。
- `Process`に`file_fds: FileFdTable` fieldを追加。RV32は`FileDesc`を
  持たないため`FileFdTable`をZST+no-opにする（`PendingLfn`と同型）。
- `Process`メソッド: `fd_mut(fd)`、`alloc_fd(desc,writable)->fd|EMFILE`、
  `close_fd(fd)->bool`、`revoke_fd_at(dir_cluster,dir_index)`。
  全て`#[cfg(not(target_arch = "riscv32"))]`。
- `ProcessTable::revoke_file_fds(loc)`: 全占有slotを走査し各processの
  `revoke_fd_at`を呼ぶ。
- `main.rs`: `FILE_FDS`/`CURRENT_PID`/`set_current_pid`/`close_all_file_fds`
  を削除し、`*mut Process`な`CURRENT_PROC`と`*mut ProcessTable`な
  `PROC_TABLE_PTR`へ置き換える。`USER_SYSCALL_PROBE_*`と同じ
  trap窓契約: run loopが`__run_user`直前に設定し、復帰後にnullへ戻す。
- `reclaim_process_slot`から`close_all_file_fds`呼び出しを除去——
  `take`でProcessがdropされfd tableも死ぬ。
- `FileFd`はplain dataなのでdrop副作用なし。所有権が型で表現される
  ことが今回の要点。

## 不変条件（変わらないもの）

- fd番号 = `FIRST_FILE_FD + slot index`、processごと4個。
- fd opはtrap窓からのみ実行され、他processのfdを参照不能。
- `unlink`は全processのfd tableを走査して失効させる。
- 方向性fd、offset前進、`pread`/`pwrite`のoffset不変。
- 全QEMU phaseの外部観測は不変（新規phaseなし）。

## 検証

- host: `Process`の`alloc_fd`/`close_fd`/`fd_mut`/`revoke_fd_at`、
  `ProcessTable::revoke_file_fds`の横断失効。
- QEMU: file-fd、file-unlink（失効経路）、file-write、file-seek、
  sched（多process共存下のfd独立）が全て現行どおり通過。
- RV32 release build適合。
