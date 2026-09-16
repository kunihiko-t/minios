//! MiniOS guestの`read_file`サンプル。
//!
//! `DOCS/NOTE.TXT`を`read_file`で読み、内容をstdoutへ書いて42で終了する。
//! syscall失敗時は70で終了する。E2Eのfile検査が使う。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{STDOUT, SyscallNumber};

/// exit異常の的内code。read_file/write失敗やpanicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// disk image fixtureの`DOCS/NOTE.TXT`。
const PATH: &[u8] = b"DOCS/NOTE.TXT";
/// guest stack上のfile buffer。fixture fileは17 byteで収まる。
const BUFFER_LEN: usize = 512;

/// MiniOS ABIの`read_file`を呼ぶ。戻り値は読んだbyte数か負のerrno。
fn sys_read_file(path: *const u8, path_len: usize, buf: *mut u8, buf_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0はpath pointer兼戻り値、a1/a2/a3/a7は引数である。path/bufはU+R/U+W検証済みの
    // stack/static上のrangeである。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a2") buf as usize,
            in("a3") buf_len,
            in("a7") SyscallNumber::ReadFile as usize,
            options(nostack),
        );
    }
    returned
}

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

/// `_start`から呼ばれるRust本体。`DOCS/NOTE.TXT`を読んでstdoutへ書く。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    let mut buffer = [0u8; BUFFER_LEN];
    let read = sys_read_file(PATH.as_ptr(), PATH.len(), buffer.as_mut_ptr(), BUFFER_LEN);
    if read < 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(STDOUT, buffer.as_ptr(), read as usize) < 0 {
        sys_exit(FAILURE_EXIT);
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
