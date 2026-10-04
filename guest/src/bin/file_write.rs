//! MiniOS guestの`create`/`write(fd)`サンプル。
//!
//! `GUEST.TXT`を作成して内容を書き込み、閉じてから読み専用で開き直して
//! 内容を検証する。writable fdへの`read`とread-only fdへの`write`が
//! `EBADF`を返すこと、directoryへの`create`が`EISDIR`を返すことも
//! 確認してから42で終了する。失敗時は70で終了する。
//! E2Eのfile-write検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{EBADF, EISDIR, FIRST_FILE_FD, STDOUT};
use minios_guest::sys::{sys_close, sys_create, sys_exit, sys_open, sys_read, sys_write};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// 作成するfile名と内容。内容はframe verifierが照合する。
const PATH: &[u8] = b"GUEST.TXT";
const DIR_PATH: &[u8] = b"DOCS";
const PAYLOAD: &[u8] = b"written by guest\n";
const BUFFER_LEN: usize = 64;

/// `_start`から呼ばれるRust本体。create→write→close→再読の契約を順に確かめる。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    // 作成して書き込む。writable fdへのreadはEBADFでなければならない。
    let fd = sys_create(PATH.as_ptr(), PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    let mut scratch = [0u8; BUFFER_LEN];
    if sys_read(fd, scratch.as_mut_ptr(), 8) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 読み専用で開き直し、書いた内容がそのまま読めることを確認する。
    let fd = sys_open(PATH.as_ptr(), PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    let mut buffer = [0u8; BUFFER_LEN];
    let read = sys_read(fd, buffer.as_mut_ptr(), BUFFER_LEN);
    if read != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if &buffer[..PAYLOAD.len()] != PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    // read-only fdへのwriteはEBADFでなければならない。
    if sys_write(fd, PAYLOAD.as_ptr(), PAYLOAD.len()) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // directoryのcreateはEISDIRで拒否される。
    if sys_create(DIR_PATH.as_ptr(), DIR_PATH.len()) != EISDIR {
        sys_exit(FAILURE_EXIT);
    }

    // 読んだ内容をstdoutへ流し、verifierへ届ける。
    if sys_write(STDOUT, PAYLOAD.as_ptr(), PAYLOAD.len()) < 0 {
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
