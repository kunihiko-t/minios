# fdベースのfile access（`open`/`read(fd)`/`close`）

## 目的

1-shotの`read_file`に対し、open中のhandle・offset読み・process終了時の
自動closeという所有権セマンティクスをguest ABIへ追加する。

## ABI（`abi/src/syscall.rs`）

- `SyscallNumber::Open = 5`: `a0=path_ptr, a1=path_len` → `a0`=fd（≥3）またはerrno
- `SyscallNumber::Close = 6`: `a0=fd` → 0またはerrno
- `read`をfd≥3へ拡張: `a0=fd, a1=buf, a2=len` → 読んだbyte数（EOFは0）、offsetを進める
- `FIRST_FILE_FD = 3`、processごと`MAX_OPEN_FILES = 4`（fd 3..=6）
- `EMFILE = -24`

## FAT32（`storage/fat32.rs`）

- `pub struct FileDesc { first_cluster: u32, size: u32 }`（Copy）
- `open_file(path) -> Result<FileDesc, FatError>`:`resolve_path`+dir拒否
- `read_range(desc, offset, output) -> Result<usize, FatError>`:
  cluster chainを`offset`までskipし、`min(size-offset, output.len())`byteを
  `output`へstreamする。`offset >= size`なら0。
- `read_file`/`for_each_entry`と同じ`not(riscv32)`ゲート。

## kernel（`user/syscall.rs`）

- `ControlSource`へ3 method追加（default`ENOSYS`）:
  `open_file(path) -> Result<usize, isize>`（fdを返す）、
  `read_fd(fd, output) -> Result<usize, isize>`、
  `close_fd(fd) -> Result<(), isize>`
- `dispatch_open`:path検証は`read_file`と同規約（EINVAL/EFAULT/UTF-8）。
- `dispatch_close`:`fd < FIRST_FILE_FD`は`EBADF`、以降はsourceへ委譲。
- `dispatch_read`:fd範囲検査（0または3..7、他は`EBADF`）→ len/range検証
  → `fd==0`はstdin経路、`fd>=3`は`read_fd`へ委譲し`ReadComplete`を返す。

## bin側fd table

- `static mut CURRENT_PID`：run loopが`USER_SYSCALL_PROBE_*`と同じ位置で
  設定・復帰。`MAX_PROCS`外ならfd opは`ENOSYS`。
- `static mut FILE_FDS: [[Option<FileFd>; MAX_OPEN_FILES]; MAX_PROCS]`、
  `FileFd { desc: FileDesc, offset: u64 }`——pidごとのslotで他processの
  fdを構造的に参照不可能にする。
- `UartControlSource::open_file/read_fd/close_fd`がtableとstorage sessionを
  仲介。`read_fd`成功時は`offset += n`。
- `reclaim_process_slot`（Exit/Fatal両経路の funnel）で`FILE_FDS[pid]`を
  clear——process終了時の自動close。run終了時に全slotを掃き直す。

## guest + harness

- `guest/src/bin/file_fd.rs`:open→分割read（offset前進）→EOF確認→close→
  close二重`EBADF`・missing`ENOENT`のerrno確認→内容をstdoutへ→exit(42)。
- `xtask`:`TestKind::FileFd`（filter`file-fd`）、disk+payload QEMU、
  frame列verifier、phase総数37。

## 非目標

- `lseek`/`pread`、書き込みfd、fdのdup、パイプ、blocking file I/O。
- stdin以外のfdへの`write`（`fd>=3`のwriteは`EBADF`のまま）。

## 検証

- host test:`read_range`のoffset/EOF/partial、dispatchの各errno経路。
- `cargo xtask test file-fd`で実機QEMU検証、全37フェーズ。
