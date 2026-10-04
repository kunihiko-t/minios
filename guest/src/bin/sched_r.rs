//! MiniOS scheduler検証用のstdin待ちguest。
//!
//! "r1\n" を出してから `read(stdin)` でblockする。入力未到着の間は
//! このprocessが選ばれず、同居するsched_a/spinとsched_b/quickが進み続ける。
//! hostがStdin frameを送るとreadが完了して "r2\n" を出してexit(5)する。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{STDIN, STDOUT};
use minios_guest::sys::{sys_exit, sys_read, sys_write};

#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    sys_write(STDOUT, b"r1\n".as_ptr(), 3);
    let mut buffer = [0u8; 16];
    sys_read(STDIN, buffer.as_mut_ptr(), buffer.len());
    sys_write(STDOUT, b"r2\n".as_ptr(), 3);
    sys_exit(5);
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
