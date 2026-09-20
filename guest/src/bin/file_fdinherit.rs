//! MiniOS guestのfd継承サンプル。
//!
//! `open`したfileのfdを`lseek`でoffset 4へ進めてから`spawn`し、childが
//! 継承したfd 3からoffset以降の13 byte（`" inside docs\n"`）をstdoutへ
//! 写すことを確認する。child側のreadがparentのoffsetを動かさないこと
//! （snapshot semantics）もparent側の再読で確かめて42で終了する。
//! childはdisk image fixtureの`DOCS/FDCHILD.ELF`で、fd 3をreadして
//! 内容をstdoutへ出す。parentが`waitpid`でblockするため、childの
//! stdoutとexit frameは必ずparentの`fd-inherit verified`より先に出る
//! ＝確定的なframe列になる。
//! 失敗時は70で終了する。E2Eのfile-fdinherit検査が使う。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{FIRST_FILE_FD, SEEK_SET, STDOUT, SyscallNumber};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。childもparentも42で終わる。
const SUCCESS_EXIT: u32 = 42;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
/// `DOCS/NOTE.TXT`の内容（17 byte）。offset 4以降が継承fd経由で読める。
const NOTE_PATH: &[u8] = b"DOCS/NOTE.TXT";
/// `DOCS/NOTE.TXT`のbyte長（`"note inside docs\n"`）。
const NOTE_LEN: usize = 17;
const CHILD_PATH: &[u8] = b"DOCS/FDCHILD.ELF";
/// offset 4以降の13 byte。childがstdoutへ写し、parent側でも再読して照合する。
const EXPECTED: &[u8] = b" inside docs\n";
const MESSAGE: &[u8] = b"fd-inherit verified\n";

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

/// MiniOS ABIの`open`を呼ぶ。戻り値はread-only fdか負のerrno。
fn sys_open(path: &[u8]) -> isize {
    sys3!(SyscallNumber::Open, path.as_ptr() as usize, path.len(), 0)
}

/// MiniOS ABIの`lseek`を呼ぶ。戻り値は新しいoffsetか負のerrno。
fn sys_lseek(fd: usize, offset: isize, whence: usize) -> isize {
    sys3!(SyscallNumber::Lseek, fd, offset, whence)
}

/// MiniOS ABIの`read`を呼ぶ。戻り値は読んだbyte数か負のerrno。
fn sys_read(fd: usize, buffer: &mut [u8], len: usize) -> isize {
    sys3!(SyscallNumber::Read, fd, buffer.as_mut_ptr() as usize, len)
}

/// MiniOS ABIの`spawn`を呼ぶ。`a0`/`a1`がELF path。戻り値はchildの
/// pidか負のerrno。childはcallerのfd tableのsnapshotを引き継ぐ。
fn sys_spawn(path: &[u8]) -> isize {
    sys3!(SyscallNumber::Spawn, path.as_ptr() as usize, path.len(), 0)
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

/// `_start`から呼ばれるRust本体。fd継承の往復を確認して42で終了する。
extern "C" fn guest_main() -> ! {
    // NOTE.TXTを開いてoffset 4へ進める。childはこの位置をsnapshotで引き継ぐ。
    let fd = sys_open(NOTE_PATH);
    if fd != FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_lseek(
        fd as usize,
        NOTE_LEN as isize - EXPECTED.len() as isize,
        SEEK_SET,
    ) != 4
    {
        sys_exit(FAILURE_EXIT);
    }

    // childを起動する。FDCHILD.ELFはfd 3をreadしてstdoutへ写し42で終了する。
    if sys_spawn(CHILD_PATH) != CHILD_PID as isize {
        sys_exit(FAILURE_EXIT);
    }

    // childの完了を待つ。parentはここでblockされ、childのstdoutとexit
    // frameが先に出てからcode 42を回収する。
    if sys1!(SyscallNumber::Waitpid, CHILD_PID) != SUCCESS_EXIT as isize {
        sys_exit(FAILURE_EXIT);
    }

    // child側のreadがparentのoffsetを動かさないこと（snapshot semantics）
    // をparent側の再読で確かめる。
    let mut buffer = [0u8; 64];
    let read = sys_read(fd as usize, &mut buffer, EXPECTED.len());
    if read != EXPECTED.len() as isize || buffer[..EXPECTED.len()] != *EXPECTED {
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
