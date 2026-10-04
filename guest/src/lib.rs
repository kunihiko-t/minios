//! MiniOS guest programが共有するuser library。
//!
//! programは`minios_guest::entry!(main)`で`_start`と`panic_handler`を生成し、
//! `main(args: Args) -> i32`の戻り値を終了codeにする。`io`、`fs`、`process`は
//! syscall 1回を1 methodへ包む薄い型で、`sys`は検証用の生のwrapperである。

#![no_std]

pub mod fs;
pub mod heap;
pub mod io;
pub mod process;
pub mod sys;

use core::{ffi::CStr, iter::FusedIterator};

/// panic時の終了code。全sample guestが失敗時に使う値と同じ。
pub const FAILURE_EXIT: i32 = 70;

/// syscallが返した負のerrno（`minios_abi::syscall::E*`の値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errno(pub isize);

/// このlibraryのsyscall結果。
pub type Result<T> = core::result::Result<T, Errno>;

/// 非負の戻り値を`Ok`、負の戻り値を`Err(Errno)`へ分ける。
fn check(returned: isize) -> Result<usize> {
    if returned < 0 {
        Err(Errno(returned))
    } else {
        Ok(returned as usize)
    }
}

/// kernelが初期stackへ積んだ`argv`を、NUL終端を除いたbyte列として順に返す。
pub struct Args {
    argv: *const *const u8,
    remaining: usize,
}

impl Args {
    /// `entry!`が`_start`の`a0/a1`から作る。
    ///
    /// # Safety
    ///
    /// `argv`は`argc`個の有効なNUL終端文字列へのpointer列で、process終了まで
    /// 書き換えられないこと。kernelの初期stack ABIがこれを保証する。
    #[doc(hidden)]
    pub unsafe fn from_raw(argc: usize, argv: *const *const u8) -> Self {
        Self {
            argv,
            remaining: argc,
        }
    }
}

impl Iterator for Args {
    type Item = &'static [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        // Safety: `from_raw`の契約により、残りの各pointerはprocess終了まで
        // 有効なNUL終端文字列を指す。
        let text = unsafe { CStr::from_ptr((*self.argv).cast()) };
        self.argv = self.argv.wrapping_add(1);
        self.remaining -= 1;
        Some(text.to_bytes())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for Args {}
impl FusedIterator for Args {}

/// `_start`と`panic_handler`を生成し、`$main`を入口にする。
///
/// `$main`は`fn(Args) -> i32`で、戻り値が`exit`の終了codeになる。panicは
/// `FAILURE_EXIT`（70）で終了する。初期`sp`はkernelが16 byte整列済みで、
/// `a0=argc`と`a1=argv`は第一・第二引数としてそのまま内部の入口関数へ流れる。
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        extern "C" fn __minios_guest_main(argc: usize, argv: *const *const u8) -> ! {
            let main: fn($crate::Args) -> i32 = $main;
            // Safety: `_start`から呼ばれ、`a0/a1`はkernelが初期stack ABIで
            // 渡した`argc`と`argv`そのものである。
            let args = unsafe { $crate::Args::from_raw(argc, argv) };
            $crate::process::exit(main(args))
        }

        /// 唯一の入口。`a0/a1`をそのまま`__minios_guest_main`へ渡す。
        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".text.entry")]
        #[unsafe(naked)]
        unsafe extern "C" fn _start() -> ! {
            ::core::arch::naked_asm!(
                "call {entry}",
                "j .",
                entry = sym __minios_guest_main,
            )
        }

        #[panic_handler]
        fn __minios_guest_panic(_info: &::core::panic::PanicInfo<'_>) -> ! {
            $crate::process::exit($crate::FAILURE_EXIT)
        }
    };
}
