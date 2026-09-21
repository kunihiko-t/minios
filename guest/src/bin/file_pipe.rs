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

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{
    EBADF, EFAULT, EPIPE, ESPIPE, FIRST_FILE_FD, SEEK_SET, STAT_KIND_PIPE, STDOUT, Stat,
    SyscallNumber,
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

macro_rules! sys1 {
    ($number:expr, $a0:expr) => {{
        let returned: isize;
        // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
        unsafe {
            asm!(
                "ecall",
                inlateout("a0") $a0 as isize => returned,
                in("a7") $number as usize,
                options(nostack),
            );
        }
        returned
    }};
}

macro_rules! sys3 {
    ($number:expr, $a0:expr, $a1:expr, $a2:expr) => {{
        let returned: isize;
        // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
        unsafe {
            asm!(
                "ecall",
                inlateout("a0") $a0 as isize => returned,
                in("a1") $a1 as isize,
                in("a2") $a2 as isize,
                in("a7") $number as usize,
                options(nostack),
            );
        }
        returned
    }};
}

/// MiniOS ABIの`pipe`を呼ぶ。`out`へ`[read_fd, write_fd]`を8 byteで
/// 書き込む。戻り値は書き込んだbyte数(8)か負のerrno。
fn sys_pipe(out: *mut [u32; 2]) -> isize {
    sys3!(SyscallNumber::Pipe, out as usize, 0, 0)
}

/// MiniOS ABIの`read`を呼ぶ。戻り値は読んだbyte数か負のerrno。
fn sys_read(fd: usize, buffer: &mut [u8], len: usize) -> isize {
    sys3!(SyscallNumber::Read, fd, buffer.as_mut_ptr() as usize, len)
}

/// MiniOS ABIの`write`を呼ぶ。戻り値は書いたbyte数か負のerrno。
fn sys_write(fd: usize, buffer: &[u8]) -> isize {
    sys3!(
        SyscallNumber::Write,
        fd,
        buffer.as_ptr() as usize,
        buffer.len()
    )
}

/// MiniOS ABIの`close`を呼ぶ。戻り値は0か負のerrno。
fn sys_close(fd: usize) -> isize {
    sys1!(SyscallNumber::Close, fd)
}

/// MiniOS ABIの`spawn`を呼ぶ。childはcallerのfd tableのsnapshotを
/// 引き継ぐ。戻り値はchildのpidか負のerrno。
fn sys_spawn(path: &[u8]) -> isize {
    sys3!(SyscallNumber::Spawn, path.as_ptr() as usize, path.len(), 0)
}

/// MiniOS ABIの`fstat`を呼ぶ。`out`へ8 byteの`Stat`を書き込む。
fn sys_fstat(fd: usize, out: &mut Stat) -> isize {
    sys3!(SyscallNumber::Fstat, fd, out as *mut Stat as usize, 0)
}

/// MiniOS ABIの`exit`を呼び、戻らない。
fn sys_exit(code: u32) -> ! {
    // Safety: ecallはkernelへtrapし、exitはprocessを終了させるため戻らない。
    unsafe {
        asm!(
            "ecall",
            in("a0") code,
            in("a7") SyscallNumber::Exit as usize,
            options(noreturn),
        );
    }
}

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
    let mut stat = Stat { size: 1, kind: 0 };
    if sys_fstat(read_fd, &mut stat) != minios_abi::syscall::STAT_LEN as isize {
        sys_exit(FAILURE_EXIT);
    }
    if stat.kind != STAT_KIND_PIPE || stat.size != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // pipe端はseekできず、read端へのwrite・write端へのreadはEBADF。
    if sys3!(SyscallNumber::Lseek, read_fd, 0, SEEK_SET) != ESPIPE {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(read_fd, b"x") != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    let mut sink = [0u8; 4];
    if sys_read(write_fd, &mut sink, 1) != EBADF {
        sys_exit(FAILURE_EXIT);
    }

    // childを起動する。PIPECH.ELFは継承したfd 3から11 byteを読み、
    // stdoutへ写して42で終了する。
    if sys_spawn(CHILD_PATH) != CHILD_PID as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(write_fd, PAYLOAD) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }

    // parentはここでblockされ、childのstdoutとexit frameが先に出てから
    // code 42を回収する。
    if sys1!(SyscallNumber::Waitpid, CHILD_PID) != SUCCESS_EXIT as isize {
        sys_exit(FAILURE_EXIT);
    }

    // write端をすべて閉じるとreadはEOF(0)を返す。childの継承したwrite端も
    // childのexitで閉じているため、parent側のcloseでwrite端は0になる。
    if sys_close(write_fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let mut eof = [0xAAu8; 4];
    if sys_read(read_fd, &mut eof, 4) != 0 {
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
    if sys_write(fds2[1] as usize, b"x") != EPIPE {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fds2[1] as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    if sys_write(STDOUT, MESSAGE) != MESSAGE.len() as isize {
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
