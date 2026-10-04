//! MiniOS scheduler検証用のguest。
//!
//! "a1\n" → yield → "a2\n" → yield → "a3\n" → yield → exit(0) と動く。
//! 最初のyieldで同居するsched_bへ順番が回るため、b列の出力がa1とa3の間へ
//! 挟まる。harnessはその交差をもって実際の切り替えを確認する。
//! 以前はbusy-waitでtimerプリエンプションを待っていたが、それではtime slice
//! の長さとQEMUの速度に結果が左右される。yieldなら切り替えの時点が
//! guestの命令列で決まる。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::STDOUT;
use minios_guest::sys::{sys_exit, sys_write, sys_yield};

#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    for marker in [b"a1\n", b"a2\n", b"a3\n"] {
        sys_write(STDOUT, marker.as_ptr(), marker.len());
        sys_yield();
    }
    sys_exit(0);
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
