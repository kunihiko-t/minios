//! MiniOS scheduler検証用のstdin待ちguest。
//!
//! "r1\n" を出してから `read(stdin)` でblockする。入力未到着の間は
//! このprocessが選ばれず、同居するsched_a/spinとsched_b/quickが進み続ける。
//! hostがStdin frameを送るとreadが完了して "r2\n" を出してexit(5)する。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{STDIN, STDOUT, SyscallNumber};

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

/// MiniOS ABIの`read`。入力未到着ならkernelがprocessをstdin待ちへ回し、
/// byte到着後に同じecallがやり直される。戻り値は受信byte数か負のerrno。
fn sys_read(fd: usize, pointer: *mut u8, len: usize) -> isize {
    let fd_argument = fd as isize;
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd_argument => returned,
            in("a1") pointer as usize,
            in("a2") len,
            in("a7") SyscallNumber::Read as usize,
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
