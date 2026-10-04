//! MiniOS scheduler検証用の短命guest。
//!
//! "b1\n".."b3\n" をすぐに出力してexit(7)する。sched_aがbusy-wait中に
//! プリエンプションで回ってくると、b列の出力がa列の途中に現れる。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::STDOUT;
use minios_guest::sys::{sys_exit, sys_write};

#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    for marker in [b"b1\n", b"b2\n", b"b3\n"] {
        sys_write(STDOUT, marker.as_ptr(), marker.len());
    }
    sys_exit(7);
}

/// 初期`sp`はkernelが16 byte整列済み。`a0/a1`はそのまま`guest_main`へ流れる。
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
    sys_exit(70);
}
