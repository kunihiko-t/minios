//! MiniOS guestの`dup2`サンプル。
//!
//! `pipe`のwrite端を`dup2`でfd 1へ移してから`DOCS/CHILD.ELF`をspawnし、
//! childのstdoutをpipeへ流す。parentは退避しておいたconsoleでfd 1を戻し、
//! `waitpid`の後にpipeのread端から`spawn-child\n`を読んで自分のstdoutへ
//! 写す。childのstdoutはconsoleへ出ないため、frame列ではchildのExitが
//! この写しより先に来る。
//! 併せて`dup2`の`EBADF`と同一fdの規約、複製したpipe端がすべて閉じるまで
//! EOFにならないこと、fd 2の`close`、16個目までの`open`と17個目の
//! `EMFILE`も確かめ、`dup verified`を出して42で終了する。
//! 失敗時は70で終了する。E2Eのuser-dup検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{
    EBADF, EMFILE, FD_TABLE_LEN, FIRST_FILE_FD, MAX_OPEN_FILES, STDERR, STDOUT,
};
use minios_guest::sys::{
    sys_close, sys_dup2, sys_exit, sys_open, sys_pipe, sys_read, sys_spawn, sys_waitpid, sys_write,
};

const FAILURE_EXIT: u32 = 70;
const SUCCESS_EXIT: u32 = 42;
/// consoleのstdoutを退避しておくfd。tableの末尾を使う。
const SAVED_STDOUT: usize = FD_TABLE_LEN - 1;
/// disk image fixtureの`DOCS/CHILD.ELF`。fd 1へ`spawn-child\n`を書き、
/// pid 1 + 41 = 42で終了する。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";
const CHILD_PID: usize = 1;
const CHILD_OUTPUT: &[u8] = b"spawn-child\n";
const OPEN_PATH: &[u8] = b"HELLO.TXT";
const MESSAGE: &[u8] = b"dup verified\n";

fn check(condition: bool) {
    if !condition {
        sys_exit(FAILURE_EXIT);
    }
}

fn pipe() -> (usize, usize) {
    let mut fds = [0u32; 2];
    check(sys_pipe(&mut fds) == 8);
    (fds[0] as usize, fds[1] as usize)
}

extern "C" fn guest_main() -> ! {
    // 範囲外や未割当のoldfdと範囲外のnewfdはEBADF、同一fdは何も変えない。
    check(sys_dup2(FD_TABLE_LEN, 5) == EBADF);
    check(sys_dup2(7, 5) == EBADF);
    check(sys_dup2(STDOUT, FD_TABLE_LEN) == EBADF);
    check(sys_dup2(STDOUT, STDOUT) == STDOUT as isize);

    // childのstdoutをpipeへ向けてからspawnし、parentのstdoutを戻す。
    let (read_fd, write_fd) = pipe();
    check(sys_dup2(STDOUT, SAVED_STDOUT) == SAVED_STDOUT as isize);
    check(sys_dup2(write_fd, STDOUT) == STDOUT as isize);
    check(sys_spawn(CHILD_PATH.as_ptr(), CHILD_PATH.len()) == CHILD_PID as isize);
    check(sys_dup2(SAVED_STDOUT, STDOUT) == STDOUT as isize);
    check(sys_close(SAVED_STDOUT) == 0);
    check(sys_close(write_fd) == 0);
    check(sys_waitpid(CHILD_PID) == SUCCESS_EXIT as isize);

    // childの出力はpipeにあり、write端はすべて閉じたので次はEOF。
    let mut buffer = [0u8; 32];
    let count = sys_read(read_fd, buffer.as_mut_ptr(), buffer.len());
    check(count == CHILD_OUTPUT.len() as isize);
    check(&buffer[..CHILD_OUTPUT.len()] == CHILD_OUTPUT);
    check(sys_read(read_fd, buffer.as_mut_ptr(), buffer.len()) == 0);
    check(sys_close(read_fd) == 0);
    check(sys_write(STDOUT, buffer.as_ptr(), count as usize) == count);

    // 複製したwrite端は元を閉じても生きており、dup2で上書きして
    // 最後のwrite端が消えるとEOFになる。
    let (read_fd, write_fd) = pipe();
    check(sys_dup2(write_fd, 10) == 10);
    check(sys_close(write_fd) == 0);
    check(sys_write(10, b"x".as_ptr(), 1) == 1);
    check(sys_read(read_fd, buffer.as_mut_ptr(), 1) == 1);
    check(sys_dup2(read_fd, 10) == 10);
    check(sys_read(read_fd, buffer.as_mut_ptr(), 1) == 0);
    check(sys_write(10, b"x".as_ptr(), 1) == EBADF);
    check(sys_close(10) == 0);
    check(sys_close(read_fd) == 0);

    // fd 2も普通のslotなので閉じられ、consoleをdup2で戻せる。
    check(sys_close(STDERR) == 0);
    check(sys_write(STDERR, b"x".as_ptr(), 1) == EBADF);
    check(sys_close(STDERR) == EBADF);
    check(sys_dup2(STDOUT, STDERR) == STDERR as isize);

    // fd 0..2を除いてMAX_OPEN_FILES個まで開け、次はEMFILE。
    for index in 0..MAX_OPEN_FILES {
        check(sys_open(OPEN_PATH.as_ptr(), OPEN_PATH.len()) == (FIRST_FILE_FD + index) as isize);
    }
    check(sys_open(OPEN_PATH.as_ptr(), OPEN_PATH.len()) == EMFILE);
    for fd in FIRST_FILE_FD..FD_TABLE_LEN {
        check(sys_close(fd) == 0);
    }

    check(sys_write(STDOUT, MESSAGE.as_ptr(), MESSAGE.len()) == MESSAGE.len() as isize);
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
