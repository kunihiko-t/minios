# `read_file` system call (process ABI storage path)

## 目的

guest processがsyscall経由でFAT32上のfileを読めるようにする。
`stdin_cat`と同じ1-shot形状で、path文字列と出力bufferをuserから
検証付きで受け取り、file内容をuser bufferへ返す。

## ABI（`abi/src/syscall.rs`）

- `SyscallNumber::ReadFile = 4`
- 引数: `a0=path_ptr, a1=path_len, a2=buf_ptr, a3=buf_len`
- 戻り値: 読んだbyte数（`buf_len`で打ち切り）、負ならerrno
- `MAX_PATH_LEN = 256`
- 追加errno: `ENOENT=-2, EIO=-5, ENOMEM=-12, ENODEV=-19, ENOTDIR=-20, EISDIR=-21`

## kernel（`user/syscall.rs`）

- `ControlSource::read_file(path, output) -> Result<usize, isize>`を追加。
  default実装は`Err(ENOSYS)`（host fixtureと非storage経路は変更不要）。
- `dispatch_read_file`:
  - `path_len`が0または`MAX_PATH_LEN`超 → `EINVAL`
  - `buf_len > MAX_READ_LEN` → `EINVAL`、`buf_len == 0` → 0
  - side effect（mountでframe確保）より先に`copy_from_user`と
    `check_user_writable_range`を済ませ`EFAULT`を確定（`dispatch_read`と同じ規約）
  - pathがUTF-8でない → `EINVAL`
  - `source.read_file`の`Ok(n)`は既存`ReadComplete{start,len}`を再利用し、
    callerが`complete_read`経由でuserへcopyする
- `read_file`がfile末尾までstreamするのはparserの契約であり、bufを
  超えた分は捨てる（戻り値は`buf_len`以下）。

## bin側storage session

- `static mut FILE_STORAGE: Option<Rv64Storage>`をmain.rsへ置き、
  stdin stagingと同じ単一hart借用パターンで`borrow_file_storage`が
  初回に`shell::probe_and_mount(&mut GlobalFrames)`を呼ぶ。
  shellのsessionとは別の第2 session（queue frameも別に所有）であり、
  静的存続でリークしない。
- `shell::{Rv64Storage, Rv64StorageError, probe_and_mount}`を`pub(crate)`化。
- `UartControlSource::read_file`でsessionを使い`session.read_file`を呼び、
  `FatError`/`Rv64StorageError`をerrnoへ写像する
  （NotFound→ENOENT、IsDirectory→EISDIR、NotDirectory→ENOTDIR、
  InvalidName→EINVAL、NoDevice/Init→ENODEV、NoFrames→ENOMEM、その他→EIO）。

## guest + harness

- `guest/src/bin/file_read.rs`:`DOCS/NOTE.TXT`を`read_file`し、内容を
  stdoutへ書いてexit(42)。失敗時はerrnoをstderrへ出してexit(70)。
- `xtask`: `TestKind::File`、guest ELFの単一image bundle、
  `qemu_command_with_payload_and_disk`（payload loader + virtio-blk）、
  frame列verifier（spawned→stdout "note inside docs"→Exit(42)→cleanup）、
  phase総数36、cli`file`配線。

## 非目標

- `open`/`close`やfd table、offset付き読み（`pread`）、書き込み、
  `read_file`によるblock/yield（sector pollはboundedで同期的）。
- RV32側のsyscall path（NEORV32はprocess ABIを持たない）。

## 検証

- host test:`dispatch_read_file`の成功・各errno・EFAULT/EINVAL経路、
  default sourceのENOSYS。
- `cargo xtask test file`で実機QEMU検証。
- 全36フェーズ + fmt + clippy + RV32 release。
