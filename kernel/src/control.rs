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
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let desc = session.create_file(path).map_err(fat_errno)?;
        // Safety: 同上。借用はこの呼び出し内で完結する。
        unsafe { crate::alloc_file_fd(desc, true) }
    }

    /// guestの`read`を開いたfdの現在offsetから読み、offsetを進める。
    /// writableなfdへのreadは`EBADF`で拒否する。
    #[cfg(target_arch = "riscv64")]
    fn read_fd(&mut self, fd: usize, output: &mut [u8]) -> Result<usize, isize> {
        use minios_abi::syscall::EBADF;

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let entry = unsafe { crate::file_fd_mut(fd) }.ok_or(EBADF)?;
        if entry.writable() {
            return Err(EBADF);
        }
        // Safety: 同上。session借用とfd借用は同じtrap窓内で完結する。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let count = session
            .read_range(entry.desc(), entry.offset(), output)
            .map_err(fat_errno)?;
        entry.advance(count as u64);
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
        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let (dir_cluster, dir_index) = session.unlink_file(path).map_err(fat_errno)?;
        // Safety: 同上。fd tableの走査はこの呼び出し内で完結する。
        unsafe { crate::revoke_file_fds(dir_cluster, dir_index) };
        Ok(())
    }

    /// guestの`lseek`をfdのoffset更新として処理する。`SEEK_END`は
    /// FileDescの現在sizeを基準にする。負になる指定と未知のwhenceは
    /// `EINVAL`で拒否する。
    #[cfg(target_arch = "riscv64")]
    fn seek_fd(&mut self, fd: usize, offset: isize, whence: usize) -> Result<u64, isize> {
        use minios_abi::syscall::{EBADF, EINVAL, SEEK_CUR, SEEK_END, SEEK_SET};

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let entry = unsafe { crate::file_fd_mut(fd) }.ok_or(EBADF)?;
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
        use minios_abi::syscall::EBADF;

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let entry = unsafe { crate::file_fd_mut(fd) }.ok_or(EBADF)?;
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
        use minios_abi::syscall::EBADF;

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let entry = unsafe { crate::file_fd_mut(fd) }.ok_or(EBADF)?;
        if !entry.writable() {
            return Err(EBADF);
        }
        // Safety: 同上。session借用とfd借用は同じtrap窓内で完結する。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        session
            .write_range(entry.desc_mut(), offset, data)
            .map_err(fat_errno)
    }

    /// guestの`write`をwritableなfdの現在offsetから書き、offsetを進める。
    /// read-onlyのfdへのwriteは`EBADF`で拒否する。
    #[cfg(target_arch = "riscv64")]
    fn write_fd(&mut self, fd: usize, data: &[u8]) -> Result<usize, isize> {
        use minios_abi::syscall::EBADF;

        // Safety: dispatch経由でtrap handlerの実行窓から呼ばれる。
        let entry = unsafe { crate::file_fd_mut(fd) }.ok_or(EBADF)?;
        if !entry.writable() {
            return Err(EBADF);
        }
        // Safety: 同上。session借用とfd借用は同じtrap窓内で完結する。
        let session = unsafe { crate::borrow_file_storage() }.map_err(storage_errno)?;
        let offset = entry.offset();
        let count = session
            .write_range(entry.desc_mut(), offset, data)
            .map_err(fat_errno)?;
        entry.advance(count as u64);
        Ok(count)
    }
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
        FatError::InvalidName | FatError::InvalidOffset => EINVAL,
        FatError::NoSpace => minios_abi::syscall::ENOSPC,
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
    crate::console::write_bytes(&header);
    crate::console::write_bytes(payload);
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
