//! MiniOS guestの最小サンプル。
//!
//! カーネルは`a0=argc`、`a1=argv`、整列済み`sp`で起動する
//! (docs/reference/minicontainer-abi.mdの初期スタックABI)。
//! このprogramはprogram nameと引数を順番どおりstdoutへ書き、42で終了する。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{STDOUT, SyscallNumber};

/// exit異常の的内code。write失敗やpanicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;

/// MiniOS ABIの`write`を呼ぶ。戻り値は書いたbyte数か負のerrno。
fn sys_write(fd: usize, pointer: *const u8, len: usize) -> isize {
    let fd_argument = fd as isize;
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0は引数兼戻り値、a1/a2/a7は引数である。
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

fn string_len(mut pointer: *const u8) -> usize {
    let mut len = 0;
    // Safety: argvの各文字列は初期スタック上のNUL終端列である。
    unsafe {
        while *pointer != 0 {
            pointer = pointer.add(1);
            len += 1;
        }
    }
    len
}

/// `_start`から`a0/a1`をそのまま受けるRust本体。
#[unsafe(no_mangle)]
extern "C" fn guest_main(argc: usize, argv: *const *const u8) -> ! {
    for index in 0..argc {
        // Safety: argvはargc個の有効なpointer列として初期スタックへ置かれている。
        let text = unsafe { *argv.add(index) };
        let len = string_len(text);
        if sys_write(STDOUT, text, len) < 0 {
            sys_exit(FAILURE_EXIT);
        }
    }
    sys_exit(SUCCESS_EXIT);
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
