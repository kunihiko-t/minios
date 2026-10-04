//! MiniOS guestの`clock`/`sleep`/`yield`サンプル。
//!
//! `clock`を読み、`sleep(50)`の後にもう一度読んで差が50 ms以上であることを
//! 確かめる。`yield`と`sleep(0)`がどちらも0を返すことも確かめ、42で終了する。
//! 失敗時は70で終了する。測った差は実行ごとに揺れるため出力しない。
//! E2Eのuser-sleep検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::STDOUT;
use minios_guest::sys::{sys_clock, sys_exit, sys_sleep, sys_write, sys_yield};

const FAILURE_EXIT: u32 = 70;
const SUCCESS_EXIT: u32 = 42;
const SLEEP_MILLIS: usize = 50;
const MESSAGE: &[u8] = b"sleep verified\n";

extern "C" fn guest_main() -> ! {
    let before = sys_clock();
    if before < 0 || sys_sleep(SLEEP_MILLIS) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let after = sys_clock();
    if after - before < SLEEP_MILLIS as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_yield() != 0 || sys_sleep(0) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(STDOUT, MESSAGE.as_ptr(), MESSAGE.len()) != MESSAGE.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    sys_exit(SUCCESS_EXIT);
}

/// 初期`sp`はkernelが16 byte整列済み。
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    naked_asm!(
        "call {entry}",
        "j .",
        entry = sym guest_main,
    )
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    sys_exit(FAILURE_EXIT);
}
