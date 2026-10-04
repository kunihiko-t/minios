//! MiniOS guestの`open`/`read(fd)`/`close`サンプル。
//!
//! `DOCS/NOTE.TXT`を開き、分割readでoffsetが進むこと、EOF、close後の
//! `EBADF`、存在しないfileの`ENOENT`を確認してから内容をstdoutへ書き、
//! 42で終了する。失敗時は70で終了する。E2Eのfile-fd検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{EBADF, ENOENT, FIRST_FILE_FD, STDOUT};
use minios_guest::sys::{sys_close, sys_exit, sys_open, sys_read, sys_write};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// disk image fixtureの`DOCS/NOTE.TXT`（"note inside docs\n"、17 byte）。
const PATH: &[u8] = b"DOCS/NOTE.TXT";
const MISSING_PATH: &[u8] = b"MISSING.TXT";
/// file内容と同じ長さの蓄積buffer。
const BUFFER_LEN: usize = 64;

/// `_start`から呼ばれるRust本体。open/read/closeの契約を順に確かめる。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    let fd = sys_open(PATH.as_ptr(), PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;

    // 分割readでoffsetが進み、最後はEOFの0を返すことを確認する。
    let mut buffer = [0u8; BUFFER_LEN];
    let mut total = 0usize;
    loop {
        let read = sys_read(fd, buffer[total..].as_mut_ptr(), 8);
        if read < 0 {
            sys_exit(FAILURE_EXIT);
        }
        if read == 0 {
            break;
        }
        total += read as usize;
    }
    if total != b"note inside docs\n".len() {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(STDOUT, buffer.as_ptr(), total) < 0 {
        sys_exit(FAILURE_EXIT);
    }

    // close済みfdと存在しないfileのerrnoを確認する。
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(fd, buffer.as_mut_ptr(), 8) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(MISSING_PATH.as_ptr(), MISSING_PATH.len()) != ENOENT {
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
