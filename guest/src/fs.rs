//! FAT32上のfileとdirectoryの操作。
//!
//! 各methodはsyscallを1回だけ呼び、結果をcacheしない。pathはUTF-8の
//! `&str`でもbyte列でも渡せる。

use core::mem::ManuallyDrop;

use minios_abi::syscall::{DIRENT_LEN, DirEnt, EIO, SEEK_CUR, SEEK_END, SEEK_SET, STAT_LEN, Stat};

use crate::{Errno, Result, check, sys};

/// `File::seek`の基準位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekFrom {
    Start(u64),
    Current(i64),
    End(i64),
}

/// 所有するfd。dropで`close`する。
///
/// kernelが`unlink`や置換`rename`でfdを失効させた後にdropすると、同じ番号を
/// 再利用した別のfdを閉じうる。失効を確かめるprobeは生のfdで行う。
#[derive(Debug)]
pub struct File {
    fd: usize,
}

impl File {
    /// read-onlyで開く。
    pub fn open(path: impl AsRef<[u8]>) -> Result<Self> {
        let path = path.as_ref();
        check(sys::sys_open(path.as_ptr(), path.len())).map(|fd| Self { fd })
    }

    /// 作成するか長さ0へ切り詰め、writableで開く。
    pub fn create(path: impl AsRef<[u8]>) -> Result<Self> {
        let path = path.as_ref();
        check(sys::sys_create(path.as_ptr(), path.len())).map(|fd| Self { fd })
    }

    /// 生のfdを所有する。`pipe`の両端や継承したfdを包むのに使う。
    pub fn from_raw_fd(fd: usize) -> Self {
        Self { fd }
    }

    /// fd番号を返す。所有は手放さない。
    pub fn as_raw_fd(&self) -> usize {
        self.fd
    }

    /// 最大`buf.len()` byteを読む。0はEOF。
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        check(sys::sys_read(self.fd, buf.as_mut_ptr(), buf.len()))
    }

    /// 書いたbyte数を返す。
    pub fn write(&mut self, buf: &[u8]) -> Result<usize> {
        check(sys::sys_write(self.fd, buf.as_ptr(), buf.len()))
    }

    /// fdのoffsetを動かし、新しいoffsetを返す。
    pub fn seek(&mut self, position: SeekFrom) -> Result<u64> {
        let (offset, whence) = match position {
            SeekFrom::Start(offset) => (offset as isize, SEEK_SET),
            SeekFrom::Current(offset) => (offset as isize, SEEK_CUR),
            SeekFrom::End(offset) => (offset as isize, SEEK_END),
        };
        check(sys::sys_lseek(self.fd, offset, whence)).map(|offset| offset as u64)
    }

    /// fdのoffsetを動かさず`offset`から読む。
    pub fn pread(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        check(sys::sys_pread(self.fd, buf.as_mut_ptr(), buf.len(), offset))
    }

    /// fdのoffsetを動かさず`offset`へ書く。
    pub fn pwrite(&self, buf: &[u8], offset: u64) -> Result<usize> {
        check(sys::sys_pwrite(self.fd, buf.as_ptr(), buf.len(), offset))
    }

    /// `fstat`でmetadataを読む。
    pub fn stat(&self) -> Result<Stat> {
        let mut out = [0u8; STAT_LEN];
        decode_stat(sys::sys_fstat(self.fd, out.as_mut_ptr()), out)
    }

    /// 所有を手放してfd番号を返す。以後dropで閉じない。
    pub fn into_raw_fd(self) -> usize {
        ManuallyDrop::new(self).fd
    }

    /// 明示的に閉じ、`close`の結果を返す。
    pub fn close(self) -> Result<()> {
        let file = ManuallyDrop::new(self);
        check(sys::sys_close(file.fd)).map(|_| ())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = sys::sys_close(self.fd);
    }
}

fn decode_stat(returned: isize, out: [u8; STAT_LEN]) -> Result<Stat> {
    if check(returned)? != STAT_LEN {
        return Err(Errno(EIO));
    }
    Ok(Stat::from_le_bytes(out))
}

/// `path`のmetadataを読む。fileとdirectoryの両方を受理する。
pub fn stat(path: impl AsRef<[u8]>) -> Result<Stat> {
    let path = path.as_ref();
    let mut out = [0u8; STAT_LEN];
    decode_stat(
        sys::sys_stat(path.as_ptr(), path.len(), out.as_mut_ptr()),
        out,
    )
}

/// directoryを作る。
pub fn mkdir(path: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    check(sys::sys_mkdir(path.as_ptr(), path.len())).map(|_| ())
}

/// 空のdirectoryを消す。
pub fn rmdir(path: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    check(sys::sys_rmdir(path.as_ptr(), path.len())).map(|_| ())
}

/// fileを消す。
pub fn unlink(path: impl AsRef<[u8]>) -> Result<()> {
    let path = path.as_ref();
    check(sys::sys_unlink(path.as_ptr(), path.len())).map(|_| ())
}

/// `from`を`to`へ移す。既存の`to`は置き換える。
pub fn rename(from: impl AsRef<[u8]>, to: impl AsRef<[u8]>) -> Result<()> {
    let (from, to) = (from.as_ref(), to.as_ref());
    check(sys::sys_rename(
        from.as_ptr(),
        from.len(),
        to.as_ptr(),
        to.len(),
    ))
    .map(|_| ())
}

/// directoryのentryをindex順に返すiterator。空pathはroot directory。
pub fn read_dir(path: &[u8]) -> ReadDir<'_> {
    ReadDir {
        path,
        index: 0,
        done: false,
    }
}

/// `read_dir`が返すiterator。1件ごとに`readdir`を1回呼び、末尾か
/// errnoで終わる。
pub struct ReadDir<'a> {
    path: &'a [u8],
    index: usize,
    done: bool,
}

impl Iterator for ReadDir<'_> {
    type Item = Result<DirEnt>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut out = [0u8; DIRENT_LEN];
        let returned = sys::sys_readdir(
            self.path.as_ptr(),
            self.path.len(),
            self.index,
            out.as_mut_ptr(),
        );
        self.index += 1;
        match check(returned) {
            Ok(0) => {
                self.done = true;
                None
            }
            Ok(DIRENT_LEN) => Some(Ok(DirEnt::from_le_bytes(out))),
            Ok(_) => {
                self.done = true;
                Some(Err(Errno(EIO)))
            }
            Err(errno) => {
                self.done = true;
                Some(Err(errno))
            }
        }
    }
}
