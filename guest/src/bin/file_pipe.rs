//! MiniOS guestのpipeサンプル。
//!
//! `pipe`でread/write両端のfdを作り、`spawn`したchildへ両端を継承させて
//! parent→childへbyteを流すことを確認する。childはdisk image fixtureの
//! `DOCS/PIPECH.ELF`で、継承したfd 3（read端）から読んだ11 byteを
//! stdoutへ写して42で終了する。parentは`waitpid`でblockするためchildの
//! stdoutとexit frameは必ずparentの`pipe verified`より先に出る＝確定的な
//! frame列になる。
//! 併せてpipe fd固有の規約——`EFAULT`・`ESPIPE`・方向違反の`EBADF`・
//! write端全閉後のEOF・read端全閉後の`EPIPE`とslot再利用——も検査する。
//! 失敗時は70で終了する。E2Eのfile-pipe検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{
    EBADF, EFAULT, EPIPE, ESPIPE, FIRST_FILE_FD, SEEK_SET, STAT_KIND_PIPE, STDOUT, Stat,
};
use minios_guest::sys::{
    sys_close, sys_exit, sys_fstat, sys_lseek, sys_pipe, sys_read, sys_spawn, sys_waitpid,
    sys_write,
};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。childもparentも42で終わる。
const SUCCESS_EXIT: u32 = 42;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
const CHILD_PATH: &[u8] = b"DOCS/PIPECH.ELF";
/// pipe経由でchildへ流すbyte列。PIPECH.ELFは11 byteを期待する。
const PAYLOAD: &[u8] = b"pipe-bytes\n";
const MESSAGE: &[u8] = b"pipe verified\n";

/// `_start`から呼ばれるRust本体。pipeの往復と端の規約を確認して42で
/// 終了する。
extern "C" fn guest_main() -> ! {
    // out pointerがuser range外ならEFAULT。sourceへ触れないことを
    // fd未割当で間接確認する。
    if sys_pipe(core::ptr::null_mut()) != EFAULT {
        sys_exit(FAILURE_EXIT);
    }

    let mut fds = [0u32; 2];
    if sys_pipe(&mut fds) != 8 {
        sys_exit(FAILURE_EXIT);
    }
    let read_fd = fds[0] as usize;
    let write_fd = fds[1] as usize;
    if read_fd != FIRST_FILE_FD || write_fd != FIRST_FILE_FD + 1 {
        sys_exit(FAILURE_EXIT);
    }

    // pipe端のmetadata規約: kind=PIPE, size=0。
    // `Stat`はrepr(C)ではないため、kernelが書くLEの8 byteを配列で受けてから復元する。
    let mut raw = [0xffu8; minios_abi::syscall::STAT_LEN];
    if sys_fstat(read_fd, raw.as_mut_ptr()) != minios_abi::syscall::STAT_LEN as isize {
        sys_exit(FAILURE_EXIT);
    }
    let stat = Stat::from_le_bytes(raw);
    if stat.kind != STAT_KIND_PIPE || stat.size != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // pipe端はseekできず、read端へのwrite・write端へのreadはEBADF。
    if sys_lseek(read_fd, 0, SEEK_SET) != ESPIPE {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(read_fd, b"x".as_ptr(), b"x".len()) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    let mut sink = [0u8; 4];
    if sys_read(write_fd, sink.as_mut_ptr(), 1) != EBADF {
        sys_exit(FAILURE_EXIT);
    }

    // childを起動する。PIPECH.ELFは継承したfd 3から11 byteを読み、
    // stdoutへ写して42で終了する。
    if sys_spawn(CHILD_PATH.as_ptr(), CHILD_PATH.len(), core::ptr::null(), 0) != CHILD_PID as isize
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(write_fd, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }

    // parentはここでblockされ、childのstdoutとexit frameが先に出てから
    // code 42を回収する。
    if sys_waitpid(CHILD_PID) != SUCCESS_EXIT as isize {
        sys_exit(FAILURE_EXIT);
    }

    // write端をすべて閉じるとreadはEOF(0)を返す。childの継承したwrite端も
    // childのexitで閉じているため、parent側のcloseでwrite端は0になる。
    if sys_close(write_fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let mut eof = [0xAAu8; 4];
    if sys_read(read_fd, eof.as_mut_ptr(), 4) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(read_fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 両端の閉じたslotは再利用される。read端だけ先に閉じたpipeへの
    // writeはEPIPEを返す。
    let mut fds2 = [0u32; 2];
    if sys_pipe(&mut fds2) != 8 {
        sys_exit(FAILURE_EXIT);
    }
    if fds2[0] as usize != FIRST_FILE_FD || fds2[1] as usize != FIRST_FILE_FD + 1 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fds2[0] as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fds2[1] as usize, b"x".as_ptr(), b"x".len()) != EPIPE {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fds2[1] as usize) != 0 {
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
