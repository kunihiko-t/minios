//! MiniOS guestのstdin転送サンプル。
//!
//! stdinをEOFまで読んでstdoutへそのまま書き、42で終了する。
//! syscall失敗時は70で終了する。E2Eのpayload-stdin検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{STDIN, STDOUT};
use minios_guest::sys::{sys_exit, sys_read, sys_write};

/// exit異常の的内code。read/write失敗やpanicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// guest stack上のread/write buffer。要求512 byteでframe境界をまたぐ。
const CHUNK_LEN: usize = 512;

/// `_start`から呼ばれるRust本体。引数は使わずstdin転送だけを行う。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    let mut chunk = [0u8; CHUNK_LEN];
    loop {
        let read = sys_read(STDIN, chunk.as_mut_ptr(), CHUNK_LEN);
        if read < 0 {
            sys_exit(FAILURE_EXIT);
        }
        if read == 0 {
            sys_exit(SUCCESS_EXIT);
        }
        if sys_write(STDOUT, chunk.as_ptr(), read as usize) < 0 {
            sys_exit(FAILURE_EXIT);
        }
    }
}

/// 初期`sp`はkernelが16 byte整列済み。`a0/a1`は第一・第二引数として
/// そのまま`guest_main`へ流れるため、register操作は不要である。
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
