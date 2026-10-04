//! stdin/stdout/stderrへの入出力と`print!`系macro。
//!
//! kernelは`write` 1回を1 frameとしてhostへ送る。`core::fmt`は1回の
//! 書式化を何度もの`write_str`へ分けるため、`print!`は1回分を
//! `PRINT_BUFFER_LEN` byteのstack bufferへ溜めてから`write`する。
//! bufferに収まる出力は1 frameになる。

use core::fmt::{self, Write};

use minios_abi::syscall::{EIO, STDERR, STDOUT};

use crate::{Errno, Result, check, sys};

/// `print!` 1回分を溜めるstack bufferの長さ。超えた分は満杯ごとに`write`する。
pub const PRINT_BUFFER_LEN: usize = 256;

/// `fd`から最大`buf.len()` byteを読む。0はEOF。
pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize> {
    check(sys::sys_read(fd, buf.as_mut_ptr(), buf.len()))
}

/// `bytes`を全部書くまで`write`を繰り返す。consoleは1回で書き切るため、
/// 1回の呼び出しが1 frameになる。空の`bytes`も`write`を1回呼ぶ。
pub fn write_all(fd: usize, mut bytes: &[u8]) -> Result<()> {
    loop {
        let written = check(sys::sys_write(fd, bytes.as_ptr(), bytes.len()))?;
        bytes = &bytes[written..];
        if bytes.is_empty() {
            return Ok(());
        }
        if written == 0 {
            return Err(Errno(EIO));
        }
    }
}

struct LineBuffer {
    fd: usize,
    len: usize,
    bytes: [u8; PRINT_BUFFER_LEN],
}

impl LineBuffer {
    fn flush(&mut self) -> fmt::Result {
        let pending = self.len;
        self.len = 0;
        write_all(self.fd, &self.bytes[..pending]).map_err(|_| fmt::Error)
    }
}

impl Write for LineBuffer {
    fn write_str(&mut self, mut text: &str) -> fmt::Result {
        while !text.is_empty() {
            if self.len == PRINT_BUFFER_LEN {
                self.flush()?;
            }
            let take = text.len().min(PRINT_BUFFER_LEN - self.len);
            self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
            self.len += take;
            text = &text[take..];
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(fd: usize, args: fmt::Arguments<'_>) {
    let mut buffer = LineBuffer {
        fd,
        len: 0,
        bytes: [0; PRINT_BUFFER_LEN],
    };
    if buffer
        .write_fmt(args)
        .and_then(|()| buffer.flush())
        .is_err()
    {
        panic!("print failed");
    }
}

#[doc(hidden)]
pub const _STDOUT: usize = STDOUT;
#[doc(hidden)]
pub const _STDERR: usize = STDERR;

/// stdoutへ書式化して書く。失敗はpanic（終了code70）になる。
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::io::_print($crate::io::_STDOUT, ::core::format_args!($($arg)*))
    };
}

/// `print!`に改行を足す。改行も同じ`write`に入る。
#[macro_export]
macro_rules! println {
    () => {
        $crate::print!("\n")
    };
    ($($arg:tt)*) => {
        $crate::io::_print(
            $crate::io::_STDOUT,
            ::core::format_args!("{}\n", ::core::format_args!($($arg)*)),
        )
    };
}

/// stderrへ書式化して書く。
#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {
        $crate::io::_print($crate::io::_STDERR, ::core::format_args!($($arg)*))
    };
}

/// `eprint!`に改行を足す。
#[macro_export]
macro_rules! eprintln {
    () => {
        $crate::eprint!("\n")
    };
    ($($arg:tt)*) => {
        $crate::io::_print(
            $crate::io::_STDERR,
            ::core::format_args!("{}\n", ::core::format_args!($($arg)*)),
        )
    };
}
