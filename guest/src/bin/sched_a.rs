//! MiniOS scheduler検証用のCPU-bound guest。
//!
//! "a1\n" → busy-wait → "a2\n" → busy-wait → "a3\n" → exit(0) と動く。
//! busy-waitの途中でtimerプリエンプションが効けば、同居するsched_bの出力が
//! a1とa3の間へ挟まる。harnessはその交差をもって実際の切り替えを確認する。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{STDOUT, SyscallNumber};

/// 1回のbusy-waitの反復数。QEMU TCGでおおよそ数百msになり、
/// 100 Hzのtimer tickを複数回またぐ長さにしてある。
const SPIN_ITERATIONS: usize = 60_000_000;

/// MiniOS ABIの`write`を呼ぶ。戻り値は書いたbyte数か負のerrno。
fn sys_write(fd: usize, pointer: *const u8, len: usize) -> isize {
    let fd_argument = fd as isize;
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd_argument => returned,
            in("a1") pointer as usize,
            in("a2") len,
            in("a7") SyscallNumber::Write as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`exit`。kernelはこの呼び出しの後guestへ戻らない。
fn sys_exit(code: u32) -> ! {
    // Safety: exitのecallはresumeしない契約のため、noreturnでよい。
    unsafe {
        asm!(
            "ecall",
            in("a0") code as isize,
            in("a7") SyscallNumber::Exit as usize,
            options(noreturn),
        );
    }
}

/// プリエンプションされても進行するbusy-wait。timer trapはuser registerを
/// 保存・復元するため、counter値は割り込みの前後で変わらない。
fn spin() {
    let mut counter = 0usize;
    while counter < SPIN_ITERATIONS {
        counter += 1;
        core::hint::black_box(counter);
    }
}

#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    for marker in [b"a1\n", b"a2\n", b"a3\n"] {
        sys_write(STDOUT, marker.as_ptr(), marker.len());
        spin();
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
