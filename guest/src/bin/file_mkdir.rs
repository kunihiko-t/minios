//! MiniOS guestの`mkdir`/`rmdir`サンプル。
//!
//! directoryの作成、duplicate作成の`EEXIST`、非空directoryの`ENOTEMPTY`、
//! fileの`rmdir`拒否、nested directory、fileを消した後のrmdir成功、
//! 削除済みdirectory内のfileが`ENOENT`になることを確認し、42で終了する。
//! 失敗時は70で終了する。E2Eのfile-mkdir検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{EEXIST, EINVAL, ENOENT, ENOTDIR, ENOTEMPTY, FIRST_FILE_FD, STDOUT};
use minios_guest::sys::{
    sys_close, sys_create, sys_exit, sys_mkdir, sys_open, sys_read, sys_rmdir, sys_unlink,
    sys_write,
};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// 作るdirectoryと、その中に置くfile。
const DIR_PATH: &[u8] = b"NEWDIR";
const INNER_PATH: &[u8] = b"NEWDIR/F.TXT";
const NESTED_PATH: &[u8] = b"DOCS/NEST";
const FILE_PATH: &[u8] = b"HELLO.TXT";
const MISSING_PATH: &[u8] = b"MISSING";
const UNDER_MISSING_PATH: &[u8] = b"MISSING/X";
const LFN_PATH: &[u8] = b"long name";
const PAYLOAD: &[u8] = b"inside dir\n";
const MESSAGE: &[u8] = b"mkdir verified\n";
const BUFFER_LEN: usize = 64;

/// `_start`から呼ばれるRust本体。directory作成→中のfile→empty検査→
/// 削除の契約を順に確かめる。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    // 作成し、中にfileを書く。
    if sys_mkdir(DIR_PATH.as_ptr(), DIR_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_create(INNER_PATH.as_ptr(), INNER_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：duplicate mkdir、非空directoryのrmdir、fileのrmdir、
    // 不在path、非8.3名。
    if sys_mkdir(DIR_PATH.as_ptr(), DIR_PATH.len()) != EEXIST {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(DIR_PATH.as_ptr(), DIR_PATH.len()) != ENOTEMPTY {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(FILE_PATH.as_ptr(), FILE_PATH.len()) != ENOTDIR {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(MISSING_PATH.as_ptr(), MISSING_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_mkdir(LFN_PATH.as_ptr(), LFN_PATH.len()) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }
    if sys_mkdir(UNDER_MISSING_PATH.as_ptr(), UNDER_MISSING_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }

    // nested作成：既存subdirectoryの下へ作れる（`..`が非root親を指す）。
    if sys_mkdir(NESTED_PATH.as_ptr(), NESTED_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(NESTED_PATH.as_ptr(), NESTED_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // fileを消せばdirectoryを外せる。中のfileもdirectoryも見えなくなる。
    if sys_unlink(INNER_PATH.as_ptr(), INNER_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(DIR_PATH.as_ptr(), DIR_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(INNER_PATH.as_ptr(), INNER_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(DIR_PATH.as_ptr(), DIR_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }

    // 削除跡slotへ同じ名前で再作成し、中のfileを読み戻して内容を照合する。
    if sys_mkdir(DIR_PATH.as_ptr(), DIR_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_create(INNER_PATH.as_ptr(), INNER_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(INNER_PATH.as_ptr(), INNER_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let mut buffer = [0u8; BUFFER_LEN];
    if sys_read(fd as usize, buffer.as_mut_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if buffer[..PAYLOAD.len()] != *PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_unlink(INNER_PATH.as_ptr(), INNER_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(DIR_PATH.as_ptr(), DIR_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    if sys_write(STDOUT, MESSAGE.as_ptr(), MESSAGE.len()) != MESSAGE.len() as isize {
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
