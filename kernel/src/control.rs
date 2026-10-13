//! UART control frameの送受信経路。Ready、Stdout、Stderr、Exit、GuestError、
//! Diagnosticの各frameを、headerの直後にpayloadが続く厳密なbyte列として
//! UARTへ載せる。hostはこの列を`minicontainer-protocol`のdecoderで検証する。
//! 受信はStdin frameだけをpull型で読み、`read`の要求分だけ配る。

use minios_abi::boot::{BOOT_ABI_MAJOR, BOOT_ABI_MINOR};
use minios_abi::control::ReadyPayload;
use minios_abi::control::{FrameHeader, FrameKind};
use minios_kernel::user::stdin::{ByteReader, StdinError, StdinStaging};
use minios_kernel::user::syscall::{ControlSink, ControlSource};

/// `dispatch_syscall`へ渡すUART sink。UARTのMMIO書き込みは失敗を返さない。
pub struct UartControlSink;

impl ControlSink for UartControlSink {
    type Error = ();

    fn frame(&mut self, kind: FrameKind, payload: &[u8]) -> Result<(), Self::Error> {
        send_frame(kind, payload);
        Ok(())
    }
}

struct UartBytes;

impl ByteReader for UartBytes {
    fn read_byte(&mut self) -> u8 {
        crate::console::read_byte()
    }

    fn try_read_byte(&mut self) -> Option<u8> {
        // 受信FIFOを空見てから読むため、この経路は決して受信待ちで停まらない。
        crate::console::stdin_pending().then(crate::console::read_byte)
    }
}

/// `dispatch_syscall`へ渡すUART source。Stdin frameをnon-blockingな
/// `try_read_byte`で引き、frame途中でbyteが尽きた場合もstagingの再開可能な
/// stateが保持される。`WouldBlock`は`Ok(None)`へ写像し、`dispatch_read`が
/// `Blocked`へ変換してprocessをstdin待ちへ回す。
/// stagingはrun単位のstaticが所有し、trapごとに借りて渡す。
pub struct UartControlSource<'a> {
    staging: &'a mut StdinStaging,
}

impl<'a> UartControlSource<'a> {
    pub const fn new(staging: &'a mut StdinStaging) -> Self {
        Self { staging }
    }
}

impl ControlSource for UartControlSource<'_> {
    type Error = StdinError;

    fn read_stdin(&mut self, output: &mut [u8]) -> Result<Option<usize>, Self::Error> {
        match self.staging.read(&mut UartBytes, output) {
            Ok(count) => Ok(Some(count)),
            Err(StdinError::WouldBlock) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// 現在processのfd tableを引き、consoleを指すentryならその向きを返す。
    /// fd番号ではなくentryで判定するため、`dup2`で移したconsoleも
    /// pipeやfileへ差し替えたfd 1も正しい経路へ流れる。process tableを
    /// 持たないuser-syscall probeでは、fd 0/1/2を固定のconsoleとみなす。
    #[cfg(target_arch = "riscv64")]
    fn console_fd(&mut self, fd: usize) -> Option<minios_kernel::user::syscall::ConsoleFd> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        if unsafe { crate::current_pid() }.is_none() {
            return minios_kernel::user::syscall::fixed_console_fd(fd);
        }
        // Safety: 同上。借用はこの呼び出し内で完結する。
        unsafe { crate::fd_entry_mut(fd) }?.console()
    }

    /// guestの`dup2`を現在processのfd tableへ委譲する。
    #[cfg(target_arch = "riscv64")]
    fn dup2(&mut self, oldfd: usize, newfd: usize) -> Result<usize, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        unsafe { crate::dup2_fd(oldfd, newfd) }
    }

    /// guestの`read_file`を遅延mount済みのstorage sessionへ委譲する。
    /// `output`より長いfileは先頭`output.len()` byteで打ち切る。
    #[cfg(target_arch = "riscv64")]
    fn read_file(&mut self, path: &str, output: &mut [u8]) -> Result<usize, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれ、借用を
        // 外へ持ち出さない。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let mut written = 0usize;
        session
            .read_file(path, |chunk| {
                let take = core::cmp::min(chunk.len(), output.len() - written);
                output[written..written + take].copy_from_slice(&chunk[..take]);
                written += take;
            })
            .map_err(fat_errno)?;
        Ok(written)
    }

    /// guestの`open`を遅延mount済みのstorage sessionとpid別fd tableへ委譲する。
    #[cfg(target_arch = "riscv64")]
    fn open_file(&mut self, path: &str) -> Result<usize, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let desc = session.open_file(path).map_err(fat_errno)?;
        // Safety: 同上。借用はこの呼び出し内で完結する。
        unsafe { crate::alloc_file_fd(desc, false) }
    }

    /// guestの`create`を遅延mount済みのstorage sessionへ委譲し、
    /// writableなfdを割り当てる。
    #[cfg(target_arch = "riscv64")]
    fn create_file(&mut self, path: &str) -> Result<usize, isize> {
        without_io_block(|| {
            // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
            let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
            let desc = session.create_file(path).map_err(fat_errno)?;
            // Safety: 同上。借用はこの呼び出し内で完結する。
            unsafe { crate::alloc_file_fd(desc, true) }
        })
    }

    /// guestの`read`を開いたfdの現在offsetから読み、offsetを進める。
    /// writableなfdへのreadは`EBADF`で拒否する。pipeのread端は
    /// `ProcessTable`のbuffer経由で読み、条件未達なら`EAGAIN`を返して
    /// callerをblockへ回す。pipeのwrite端へのreadは`EBADF`。
    #[cfg(target_arch = "riscv64")]
    fn read_fd(&mut self, fd: usize, output: &mut [u8]) -> Result<usize, isize> {
        use minios_abi::syscall::EBADF;
        use minios_kernel::process::FdEntry;

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let entry = unsafe { crate::fd_entry_mut(fd) }.ok_or(EBADF)?;
        let file = match entry {
            FdEntry::File(file) => file,
            // consoleはdispatchが`console_fd`で先に振り分ける。
            FdEntry::Console(_) | FdEntry::Pipe { write: true, .. } => return Err(EBADF),
            // Safety: 同上。table借用はこの呼び出し内で完結する。
            FdEntry::Pipe { id, .. } => return unsafe { crate::pipe_read(*id, output) },
        };
        if file.writable() {
            return Err(EBADF);
        }
        // Safety: 同上。session借用とfd借用は同じtrap窓内で完結する。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let count = session
            .read_range(file.desc(), file.offset(), output)
            .map_err(fat_errno)?;
        file.advance(count as u64);
        Ok(count)
    }

    /// guestの`close`をpid別fd tableへ委譲する。
    #[cfg(target_arch = "riscv64")]
    fn close_fd(&mut self, fd: usize) -> Result<(), isize> {
        use minios_abi::syscall::EBADF;

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        if unsafe { crate::close_file_fd(fd) } {
            Ok(())
        } else {
            Err(EBADF)
        }
    }

    /// guestの`unlink`をstorage sessionへ委譲し、削除したentryを指す
    /// 全processのfdを失効させる。clusterは即座に解放されるため、
    /// fdを残すと他fileのentryや内容を壊し得る。
    #[cfg(target_arch = "riscv64")]
    fn unlink(&mut self, path: &str) -> Result<(), isize> {
        without_io_block(|| {
            // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
            let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
            let (dir_cluster, dir_index) = session.unlink_file(path).map_err(fat_errno)?;
            // Safety: 同上。fd tableの走査はこの呼び出し内で完結する。
            unsafe { crate::revoke_file_fds(dir_cluster, dir_index) };
            Ok(())
        })
    }

    /// guestの`rename`をstorage sessionへ委譲する。in-place改名では
    /// source fileのfdはそのまま有効で、cross-directory moveでは
    /// entryの物理位置が変わるため全processのfdを新位置へ追従させる。
    /// 置き換えられたtargetのclusterは解放されるため、そのfdは失効。
    #[cfg(target_arch = "riscv64")]
    fn rename(&mut self, old_path: &str, new_path: &str) -> Result<(), isize> {
        without_io_block(|| {
            // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
            let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
            let outcome = session.rename(old_path, new_path).map_err(fat_errno)?;
            if let Some(((oc, oi), (nc, ni))) = outcome.moved {
                // Safety: 同上。fd tableの走査はこの呼び出し内で完結する。
                unsafe { crate::relocate_file_fds(oc, oi, nc, ni) };
            }
            if let Some((dir_cluster, dir_index)) = outcome.replaced {
                // Safety: 同上。fd tableの走査はこの呼び出し内で完結する。
                unsafe { crate::revoke_file_fds(dir_cluster, dir_index) };
            }
            Ok(())
        })
    }

    /// guestの`getpid`を現在processへ委譲する。trap中のprocessが
    /// なければ`ENOSYS`。
    #[cfg(target_arch = "riscv64")]
    fn getpid(&mut self) -> isize {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        match unsafe { crate::current_pid() } {
            Some(pid) => pid as isize,
            None => minios_abi::syscall::ENOSYS,
        }
    }

    /// guestの`spawn`をstorage sessionとprocess tableへ委譲する。
    /// `path`のfileをELFとして読み込み、新processを生成してtableへ登録し、
    /// 採番pidを返す。`argv`が空ならbasenameをprocess名とargv[0]にし、
    /// 空でなければ`argv[0]`をprocess名、`argv`全体をchildのargvにする。
    #[cfg(target_arch = "riscv64")]
    fn spawn(&mut self, path: &str, argv: &[&str]) -> Result<usize, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれ、借用を
        // 外へ持ち出さない。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let mut elf = alloc::vec::Vec::new();
        session
            .read_file(path, |chunk| elf.extend_from_slice(chunk))
            .map_err(fat_errno)?;

        // `Process.name`は`&'static str`必須のため、argv[0]をleakする。
        // spawn回数比例の小さいleakとして明示的に許容する。
        let (name, arguments) = match argv.split_first() {
            Some((first, rest)) => (*first, rest),
            None => (path.rsplit('/').next().unwrap_or(path), &[][..]),
        };
        let name = alloc::string::String::from(name).leak();

        // Safety: 同上。返り値のprocess所有権はinsertか回収まで持つ。
        let process = match unsafe { crate::spawn_process(name, &elf, arguments) } {
            Ok(process) => process,
            Err(failure) => {
                // 回収しきれなかったimageが残っていればdestroyを一度試す。
                if let Some(image) = failure.image {
                    let mut frames = crate::GlobalFrames;
                    let _ = image.destroy(&mut frames);
                }
                return Err(match failure.error {
                    minios_kernel::process::SpawnError::Load(_) => minios_abi::syscall::EINVAL,
                    _ => minios_abi::syscall::ENOMEM,
                });
            }
        };
        // Safety: 同上。table満杯はprocessを返すため所有frameを回収する。
        match unsafe { crate::insert_spawned(process) } {
            Ok(pid) => Ok(pid),
            Err(mut process) => {
                let mut frames = crate::GlobalFrames;
                let _ = process.reclaim(&mut frames);
                Err(minios_abi::syscall::ENOMEM)
            }
        }
    }

    /// guestの`waitpid`をprocess tableへ委譲する。`Ok(Some(code))`は
    /// 回収した終了code、`Ok(None)`は対象がliveでblockへ移すこと、
    /// `Err`は`ECHILD`/`EINVAL`/`ENOSYS`である。
    #[cfg(target_arch = "riscv64")]
    fn waitpid(&mut self, pid: usize) -> Result<Option<u32>, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        unsafe { crate::wait_pid(pid) }
    }

    /// guestの`kill`をprocess tableへ委譲する。`Err`は`ECHILD`/`EINVAL`
    /// `ENOSYS`である。対象の回収と`waitpid`への終了code通知は
    /// run loop側が行う。
    #[cfg(target_arch = "riscv64")]
    fn kill(&mut self, pid: usize) -> Result<(), isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        unsafe { crate::kill_pid(pid) }
    }

    /// guestの`exec`をstorage sessionと現在processへ委譲する。`path`の
    /// fileをELFとして読み込み、basenameをprocess名として呼び出し
    /// processのimageを置き替える。成功時は新imageの初期contextを返し、
    /// 旧imageは`retired_image`へ退避されてrun loopが解放する。
    #[cfg(target_arch = "riscv64")]
    fn exec(&mut self, path: &str) -> Result<minios_kernel::user::UserContext, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれ、借用を
        // 外へ持ち出さない。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let mut elf = alloc::vec::Vec::new();
        session
            .read_file(path, |chunk| elf.extend_from_slice(chunk))
            .map_err(fat_errno)?;

        // `spawn`と同じく`Process.name`は`&'static str`必須のため、
        // basenameをleakする。
        let name = path.rsplit('/').next().unwrap_or(path);
        let name = alloc::string::String::from(name).leak();

        // Safety: 同上。失敗してもprocessは旧imageのまま残る。
        match unsafe { crate::exec_current_process(name, &elf) } {
            Ok(context) => Ok(context),
            Err(failure) => {
                // 回収しきれなかったimageが残っていればdestroyを一度試す。
                if let Some(image) = failure.image {
                    let mut frames = crate::GlobalFrames;
                    let _ = image.destroy(&mut frames);
                }
                Err(match failure.error {
                    minios_kernel::process::SpawnError::Load(_) => minios_abi::syscall::EINVAL,
                    _ => minios_abi::syscall::ENOMEM,
                })
            }
        }
    }

    /// guestの`stat`をstorage sessionへ委譲する。fileとdirectoryの両方を
    /// 受理し、sizeとkindをABIの`Stat`へ写像する。
    #[cfg(target_arch = "riscv64")]
    fn stat(&mut self, path: &str) -> Result<minios_abi::syscall::Stat, isize> {
        use minios_abi::syscall::{STAT_KIND_DIR, STAT_KIND_FILE, Stat};

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let info = session.stat(path).map_err(fat_errno)?;
        Ok(Stat {
            size: info.size,
            kind: if info.directory {
                STAT_KIND_DIR
            } else {
                STAT_KIND_FILE
            },
        })
    }

    /// guestの`fstat`をfdのentryから返す。file fdは`FileDesc`が保持する
    /// size（open時の値を`write_range`が更新する）を返すため、sessionへ
    /// 触れずに済む。pipe端とconsoleはsize 0でそれぞれの`STAT_KIND_*`を
    /// 返す。未割当fdは`EBADF`。
    #[cfg(target_arch = "riscv64")]
    fn fstat(&mut self, fd: usize) -> Result<minios_abi::syscall::Stat, isize> {
        use minios_abi::syscall::{EBADF, STAT_KIND_FILE, Stat};

        use minios_kernel::process::FdEntry;

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let entry = unsafe { crate::fd_entry_mut(fd) }.ok_or(EBADF)?;
        Ok(match entry {
            FdEntry::File(file) => Stat {
                size: file.desc().size(),
                kind: STAT_KIND_FILE,
            },
            FdEntry::Pipe { .. } => Stat {
                size: 0,
                kind: minios_abi::syscall::STAT_KIND_PIPE,
            },
            FdEntry::Console(_) => Stat {
                size: 0,
                kind: minios_abi::syscall::STAT_KIND_CONSOLE,
            },
        })
    }

    /// guestの`readdir`をstorage sessionへ委譲する。`""`はrootを指し、
    /// index番目のentryを`DirEnt`へ写像する。末尾超過は`Ok(None)`。
    #[cfg(target_arch = "riscv64")]
    fn readdir(
        &mut self,
        path: &str,
        index: usize,
    ) -> Result<Option<minios_abi::syscall::DirEnt>, isize> {
        use minios_abi::syscall::{DIRENT_NAME_LEN, DirEnt, STAT_KIND_DIR, STAT_KIND_FILE};

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let Some(entry) = session.entry_at(path, index).map_err(fat_errno)? else {
            return Ok(None);
        };
        let name = entry.name().as_bytes();
        let mut buffer = [0u8; DIRENT_NAME_LEN];
        buffer[..name.len()].copy_from_slice(name);
        Ok(Some(DirEnt {
            name_len: name.len() as u32,
            kind: if entry.is_directory() {
                STAT_KIND_DIR
            } else {
                STAT_KIND_FILE
            },
            name: buffer,
        }))
    }

    /// guestの`mkdir`をstorage sessionへ委譲する。dir作成はfdを返さず、
    /// 失効させるfdもない。
    #[cfg(target_arch = "riscv64")]
    fn make_dir(&mut self, path: &str) -> Result<(), isize> {
        without_io_block(|| {
            // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
            let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
            session.create_dir(path).map_err(fat_errno)
        })
    }

    /// guestの`rmdir`をstorage sessionへ委譲する。dirはfdを持てないため
    /// 戻り値の削除位置に失効させる対象はない。
    #[cfg(target_arch = "riscv64")]
    fn remove_dir(&mut self, path: &str) -> Result<(), isize> {
        without_io_block(|| {
            // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
            let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
            session.remove_dir(path).map_err(fat_errno)?;
            Ok(())
        })
    }

    /// guestの`lseek`をfdのoffset更新として処理する。`SEEK_END`は
    /// FileDescの現在sizeを基準にする。負になる指定と未知のwhenceは
    /// `EINVAL`で拒否する。
    #[cfg(target_arch = "riscv64")]
    fn seek_fd(&mut self, fd: usize, offset: isize, whence: usize) -> Result<u64, isize> {
        use minios_abi::syscall::{EBADF, EINVAL, ESPIPE, SEEK_CUR, SEEK_END, SEEK_SET};

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let Some(entry) = unsafe { crate::fd_entry_mut(fd) }.ok_or(EBADF)?.file_mut() else {
            return Err(ESPIPE);
        };
        let base: u64 = match whence {
            SEEK_SET => 0,
            SEEK_CUR => entry.offset(),
            SEEK_END => entry.desc().size() as u64,
            _ => return Err(EINVAL),
        };
        let next = base as i128 + offset as i128;
        if next < 0 || next > u64::MAX as i128 {
            return Err(EINVAL);
        }
        entry.set_offset(next as u64);
        Ok(entry.offset())
    }

    /// guestの`pread`をfdのfileの明示offsetから読む。fd保持のoffsetと
    /// 方向性（writable fdは`EBADF`）は`read`と同じ規約で扱う。
    #[cfg(target_arch = "riscv64")]
    fn pread_fd(&mut self, fd: usize, offset: u64, output: &mut [u8]) -> Result<usize, isize> {
        use minios_abi::syscall::{EBADF, ESPIPE};

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let Some(entry) = unsafe { crate::fd_entry_mut(fd) }.ok_or(EBADF)?.file_mut() else {
            return Err(ESPIPE);
        };
        if entry.writable() {
            return Err(EBADF);
        }
        // Safety: 同上。session借用とfd借用は同じtrap窓内で完結する。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        session
            .read_range(entry.desc(), offset, output)
            .map_err(fat_errno)
    }

    /// guestの`pwrite`をfdのfileの明示offsetへ書く。fd保持のoffsetと
    /// 方向性（read-only fdは`EBADF`）は`write`と同じ規約で扱う。
    /// FileDescのsize/first_clusterは`write_range`が更新する。
    #[cfg(target_arch = "riscv64")]
    fn pwrite_fd(&mut self, fd: usize, offset: u64, data: &[u8]) -> Result<usize, isize> {
        use minios_abi::syscall::{EBADF, ESPIPE};

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let Some(entry) = unsafe { crate::fd_entry_mut(fd) }.ok_or(EBADF)?.file_mut() else {
            return Err(ESPIPE);
        };
        if !entry.writable() {
            return Err(EBADF);
        }
        without_io_block(|| {
            // Safety: 同上。session借用とfd借用は同じtrap窓内で完結する。
            let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
            session
                .write_range(entry.desc_mut(), offset, data)
                .map_err(fat_errno)
        })
    }

    /// guestの`write`をwritableなfdの現在offsetから書き、offsetを進める。
    /// read-onlyのfdへのwriteは`EBADF`で拒否する。pipeのwrite端は
    /// `ProcessTable`のbufferへ書き、満杯なら`EAGAIN`を返してcallerを
    /// blockへ回す。pipeのread端へのwriteは`EBADF`。
    #[cfg(target_arch = "riscv64")]
    fn write_fd(&mut self, fd: usize, data: &[u8]) -> Result<usize, isize> {
        use minios_abi::syscall::EBADF;
        use minios_kernel::process::FdEntry;

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let entry = unsafe { crate::fd_entry_mut(fd) }.ok_or(EBADF)?;
        let file = match entry {
            FdEntry::File(file) => file,
            // consoleはdispatchが`console_fd`で先に振り分ける。
            FdEntry::Console(_) | FdEntry::Pipe { write: false, .. } => return Err(EBADF),
            // Safety: 同上。table借用はこの呼び出し内で完結する。
            FdEntry::Pipe { id, .. } => return unsafe { crate::pipe_write(*id, data) },
        };
        if !file.writable() {
            return Err(EBADF);
        }
        without_io_block(|| {
            // Safety: 同上。session借用とfd借用は同じtrap窓内で完結する。
            let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
            let offset = file.offset();
            let count = session
                .write_range(file.desc_mut(), offset, data)
                .map_err(fat_errno)?;
            file.advance(count as u64);
            Ok(count)
        })
    }

    /// guestの`pipe`をpipe tableとfd tableへ委譲する。両端のfdを
    /// 返し、`spawn`したchildがそのまま継承できる。
    #[cfg(target_arch = "riscv64")]
    fn create_pipe(&mut self) -> Result<(usize, usize), isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        unsafe { crate::create_pipe() }
    }

    /// guestの`sbrk`を現在processのimageへ委譲する。
    #[cfg(target_arch = "riscv64")]
    fn sbrk(&mut self, increment: isize) -> Result<u64, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        unsafe { crate::sbrk_current_process(increment) }
    }

    /// guestの`clock`へtimer tickから求めた経過millisecondを返す。
    #[cfg(target_arch = "riscv64")]
    fn clock(&mut self) -> isize {
        isize::try_from(crate::time::uptime_millis()).unwrap_or(isize::MAX)
    }

    /// guestの`sleep`で現在processを起床tickまでのsleep状態へmarkする。
    #[cfg(target_arch = "riscv64")]
    fn sleep(&mut self, millis: usize) -> Result<(), isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        unsafe { crate::sleep_current_process(millis) }
    }

    /// guestの`socket`をnetwork stackのtableへ委譲し、呼び出しprocessの
    /// pidを所有者として記録する。table満杯は`EMFILE`、net device未接続は
    /// `borrow_net_stack`の`ENODEV`。process contextを持たない呼び出しは
    /// `ENOSYS`。
    #[cfg(target_arch = "riscv64")]
    fn socket(&mut self) -> Result<usize, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let pid = unsafe { crate::current_pid() }.ok_or(minios_abi::syscall::ENOSYS)?;
        let stack = unsafe { crate::borrow_net_stack() }?;
        stack.socket_open(pid).ok_or(minios_abi::syscall::EMFILE)
    }

    /// guestの`sendto`をnetwork stackへ委譲し、成功時は送信byte数を返す。
    /// socketの所有者でない呼び出しはstack側の`BadSocket`→`EBADF`で拒否する。
    #[cfg(target_arch = "riscv64")]
    fn send_to(
        &mut self,
        sock: usize,
        data: &[u8],
        dst_ip: [u8; 4],
        dst_port: u16,
    ) -> Result<usize, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let pid = unsafe { crate::current_pid() }.ok_or(minios_abi::syscall::ENOSYS)?;
        let stack = unsafe { crate::borrow_net_stack() }?;
        stack
            .socket_send_to(pid, sock, dst_ip, dst_port, data)
            .map(|()| data.len())
            .map_err(net_errno)
    }

    /// guestの`recvfrom`をnetwork stackへ委譲する。queue空ならcallerを
    /// `BlockedOnIo`へmarkして`EAGAIN`を返し、virtio-netの割り込みで
    /// 起きたretryが`stack.poll()`で新着frameをqueueへ取り込む。
    /// socketの所有者でない呼び出しは`BadSocket`→`EBADF`で拒否する。
    #[cfg(target_arch = "riscv64")]
    fn recv_from(
        &mut self,
        sock: usize,
        output: &mut [u8],
    ) -> Result<Option<([u8; 4], u16, usize)>, isize> {
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let pid = unsafe { crate::current_pid() }.ok_or(minios_abi::syscall::ENOSYS)?;
        let stack = unsafe { crate::borrow_net_stack() }?;
        stack.poll().map_err(net_errno)?;
        match stack.socket_recv_from(pid, sock, output) {
            Ok(None) => {
                // disk I/Oの`WouldBlock`と同じく、割り込み完了待ちで
                // callerをmarkしてから`EAGAIN`へ流す。
                unsafe { crate::block_current_on_io() };
                Err(minios_abi::syscall::EAGAIN)
            }
            Ok(received) => Ok(received),
            Err(error) => Err(net_errno(error)),
        }
    }
}

/// network stackの失敗をguest向けerrnoへ写像する。
#[cfg(target_arch = "riscv64")]
fn net_errno(error: minios_kernel::net::stack::StackError) -> isize {
    use minios_abi::syscall::{EBADF, EINVAL, EIO, EMFILE};
    use minios_kernel::net::stack::StackError;

    match error {
        StackError::BadSocket => EBADF,
        StackError::PacketTooLarge => EINVAL,
        StackError::NoSocket => EMFILE,
        StackError::Timeout | StackError::Link(_) => EIO,
    }
}

/// FSを変更するopは`WouldBlock`で途中退避できない。部分commitを残した
/// ままsyscallをやり直すと、二回目が初回の成果物を見て失敗するため、
/// opの間だけ`io_block_allowed`を下げてdriverがrequestをinlineで完結
/// させる。戻り値を返す前にflagを復帰する。
#[cfg(target_arch = "riscv64")]
fn without_io_block<T>(op: impl FnOnce() -> T) -> T {
    let prev = crate::storage::virtio_blk::io_block_allowed();
    crate::storage::virtio_blk::set_io_block_allowed(false);
    let out = op();
    crate::storage::virtio_blk::set_io_block_allowed(prev);
    out
}

/// probe/mountの失敗をguest向けerrnoへ写像する。
#[cfg(target_arch = "riscv64")]
fn storage_errno(error: crate::shell::Rv64StorageError) -> isize {
    use crate::shell::Rv64StorageError;
    use minios_abi::syscall::{ENODEV, ENOMEM};

    match error {
        Rv64StorageError::NoDevice | Rv64StorageError::Init(_) => ENODEV,
        Rv64StorageError::NoFrames => ENOMEM,
        Rv64StorageError::Fat(error) => fat_errno(error),
    }
}

/// FAT32/parserの失敗をguest向けerrnoへ写像する。
#[cfg(target_arch = "riscv64")]
fn fat_errno(
    error: minios_kernel::storage::fat32::FatError<crate::storage::virtio_blk::VirtioError>,
) -> isize {
    use minios_abi::syscall::{EINVAL, EIO, EISDIR, ENOENT, ENOTDIR};
    use minios_kernel::storage::fat32::FatError;

    match error {
        FatError::NotFound => ENOENT,
        FatError::IsDirectory => EISDIR,
        FatError::NotDirectory => ENOTDIR,
        FatError::InvalidName | FatError::InvalidOffset | FatError::MoveIntoItself => EINVAL,
        FatError::Exists => minios_abi::syscall::EEXIST,
        FatError::NotEmpty => minios_abi::syscall::ENOTEMPTY,
        FatError::NoSpace => minios_abi::syscall::ENOSPC,
        // `WouldBlock`はin-flightなdisk I/Oの完了待ち。callerを
        // `BlockedOnIo`へmarkして`EAGAIN`を返し、dispatchの`Blocked`
        // 経路でecallをやり直す。guestへ`EAGAIN`自体が返ることはない。
        FatError::Read(crate::storage::virtio_blk::VirtioError::WouldBlock) => {
            // Safety: trap handlerの実行窓から呼ばれる。
            unsafe { crate::block_current_on_io() };
            minios_abi::syscall::EAGAIN
        }
        FatError::Read(_)
        | FatError::Unsupported
        | FatError::InvalidFilesystem
        | FatError::CorruptChain => EIO,
    }
}

fn send_frame(kind: FrameKind, payload: &[u8]) {
    let header = FrameHeader {
        kind,
        payload_len: payload.len() as u32,
    }
    .encode();
    // headerを送ってからpayloadを送る順序を、host側decoderの契約として守る。
    // frameが他hartの出力に割り込まれないよう1 lock区間で書く。
    crate::console::write_frame(&header, payload);
}

/// guestの実行準備が整ったことをhostへ通知し、以降のUARTをcontrol frameへ限定する。
/// QEMU user testのkernelはpayload経由でだけ呼ぶため、このTaskでは未使用である。
#[allow(dead_code)]
pub fn send_ready() {
    let payload = ReadyPayload {
        abi_major: BOOT_ABI_MAJOR,
        abi_minor: BOOT_ABI_MINOR,
    }
    .encode();
    send_frame(FrameKind::Ready, &payload);
    // Ready以降はplain console textを混在させない。
    crate::console::enter_control_mode();
}

pub fn send_guest_error(message: &[u8]) {
    send_frame(FrameKind::GuestError, message);
}

pub fn send_diagnostic(message: &[u8]) {
    send_frame(FrameKind::Diagnostic, message);
}
