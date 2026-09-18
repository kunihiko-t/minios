# spawn/getpid（guestからの動的process生成）

2026-09-18

## ゴール

guestが`getpid`で自分のpidを得、`spawn(path)`でFAT32上のELF fileから
新しいprocessを実行中に起こせるようにする。動的pidを持つheap-backed
`ProcessTable`とstorage sessionの整備済み基盤を組み合わせる。

`waitpid`/終了code受け渡しは非目標——spawned childはfire-and-forgetで、
既存のround-robinとreclaim経路がそのまま回収する。

## ABI

- `Getpid = 15`：戻り値は呼び出しprocessのpid（負にならない）。
- `Spawn = 16`：`a0=path pointer`、`a1=path length`。
  戻り値はchildのpid（正）か負のerrno。

## 設計

### `getpid`

`CURRENT_PROC`（trap窓の`*mut Process`）の`pid`を返す。
`ControlSource::getpid(&mut self) -> isize`としてsyscall層へ露出し、
bin実装が`current_pid()` accessor経由で読む。storage不要。

### `spawn`

`ControlSource::spawn(&mut self, path: &str) -> Result<usize, isize>`：

1. `copy_user_path`規約でpathを検証（UTF-8・`is_valid_component`・長さ）。
2. storage sessionから`read_file`でELF byte列を取得（`Vec<u8>`へ収集）。
   `ENOENT`/`EISDIR`等は`fat_errno`写像。
3. `Process::spawn`をtrap窓内で呼ぶ。必要なresourceは：
   - `allocator`：`GlobalFrames`（global `FrameSource`）。
   - `memory`：`USER_SYSCALL_PROBE_MEMORY`が指す`IdentityFrameStore`。
   - `kernel_mappings`：新global `KERNEL_MAP_PLAN_PTR`経由の
     `KernelMapPlan::mappings()`。`run_boot_payload`が`set_process_table`
     と同じ箇所で設定し、loop終了時に解除する。
   - `name`：pathのbasename（`/`以降）を`alloc::string::String::leak`で
     `&'static str`化。`Process.name`が`&'static str`必須のため。
     spawn回数比例の小さいleakとして明示する。
   - `arguments`：`&[]`（argv[0]=nameのみ）。
4. `table.insert(process)`でpid採番して返す。table満杯は`ENOMEM`、
   ELF load失敗は`EINVAL`、frame不足は`ENOMEM`。

### Vec再配置の安全性（重要）

`table.insert`が`Vec`を伸長すると、run loopが保持する`CURRENT_PROC`/
`process` raw pointerがdangleする。`ProcessTable::new`を
`Vec::with_capacity(MAX_PROCS)`へ変え、admission cap内ではreallocが
構造的に起きないことを不変条件にする。`insert`は末尾pushで既存要素を
動かさず、`remove`（reclaim）はdispatch窓の外でのみ起きるため、
窓内では`procs`の各要素アドレスが安定する。

### 子processの実行

insertされたprocessは`Runnable`で即座にround-robin対象になり、
親のdispatchが戻ると以後の`pick_next`で選ばれる。stdin/stdoutは全
processで共有。`ProcExit` frameは`process.name()`を使うため
manifest v2経路で個別に報告される。

## guest `file_spawn`

1. `getpid`が0以上を返す。
2. `spawn("CHILD.ELF")`が親より大きいpidを返す。
3. `spawn("MISSING.ELF")`が`ENOENT`、dir pathが`EISDIR`、
   非8.3名が`EINVAL`を返す。
4. 親は`"spawn verified"`を出してexit 42。
5. CHILD.ELF（disk image内の別guest ELF）は`"spawn child"`を
   stdoutへ出してexit 0——control frameで個別ProcExitとして観測する。

`xtask`のdisk builderにCHILD.ELFを追加し、qemu.rsに新phase
`FileSpawn`とexpected frame列を配線する。

## host test

- `ProcessTable::with_capacity`：MAX_PROCS満杯まで`procs.as_ptr()`が
  不変（要素アドレス安定）。
- syscall層：`Getpid`が`source.getpid()`を返す、`Spawn`がpath検証と
  source委譲を行う、storage無しで`ENOSYS`。
- fat32層の変更は無し。

## docs

- `minicontainer-abi.md`：syscall表に15/16、spawn意味論（fire-and-
  forget、`ENOMEM`/`EINVAL`/`ENOENT`）。
- `architecture.md`：動的spawn経路とVec安定性の不変条件。
- `11-test-harness.md`：`file-spawn` phase（全43）。
- `10-shell.md`：変更無し。
