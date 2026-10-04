//! MiniOS guestの`unlink`サンプル。
//!
//! fileを作成して書き込み、`unlink`で削除する。削除したentryを指すfdが
//! `EBADF`で失効すること、削除済みのfileが`ENOENT`、directoryへの
//! `unlink`が`EISDIR`を返すことを確認し、同名で作り直して内容を読み
//! 戻してから42で終了する。失敗時は70で終了する。
//! E2Eのfile-unlink検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{EBADF, EISDIR, ENOENT, FIRST_FILE_FD, STDOUT};
use minios_guest::sys::{
    sys_close, sys_create, sys_exit, sys_open, sys_read, sys_unlink, sys_write,
};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// 作成・削除・再作成するfile名と内容。内容はframe verifierが照合する。
const PATH: &[u8] = b"UNLINK.TXT";
const DIR_PATH: &[u8] = b"DOCS";
const MISSING_PATH: &[u8] = b"MISSING.TXT";
const PAYLOAD: &[u8] = b"file removed\n";
const BUFFER_LEN: usize = 64;

/// `_start`から呼ばれるRust本体。create→write→unlink→fd失効→再作成の
/// 契約を順に確かめる。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    // 作成して書き込み、閉じる。
    let fd = sys_create(PATH.as_ptr(), PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 読み専用で開いたままunlinkする。開いているfdは即座に失効する。
    let fd = sys_open(PATH.as_ptr(), PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_unlink(PATH.as_ptr(), PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let mut buffer = [0u8; BUFFER_LEN];
    if sys_read(fd, buffer.as_mut_ptr(), 8) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != EBADF {
        sys_exit(FAILURE_EXIT);
    }

    // 削除済みのfileはENOENT、directoryと不在pathへのunlinkはerrnoを返す。
    if sys_open(PATH.as_ptr(), PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_unlink(PATH.as_ptr(), PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_unlink(DIR_PATH.as_ptr(), DIR_PATH.len()) != EISDIR {
        sys_exit(FAILURE_EXIT);
    }
    if sys_unlink(MISSING_PATH.as_ptr(), MISSING_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }

    // 削除跡slotを再利用して同名で作り直し、新しい内容が読めることを
    // 確認する。
    let fd = sys_create(PATH.as_ptr(), PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(PATH.as_ptr(), PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    let read = sys_read(fd, buffer.as_mut_ptr(), BUFFER_LEN);
    if read != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if &buffer[..PAYLOAD.len()] != PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 読み戻した内容をstdoutへ流し、verifierへ届ける。
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
