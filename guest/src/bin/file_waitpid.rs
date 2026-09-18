//! MiniOS guestの`waitpid`サンプル。
//!
//! `spawn`で`DOCS/CHILD.ELF`を起動し、`waitpid`がchildの終了code 42を
//! 返すこと、reap済みpid・自分自身・存在しないpidが`ECHILD`を返すことを
//! 確認して42で終了する。childは`spawn-child`をstdoutへ書き`pid + 41` =
//! 42で終了する（disk image fixtureの`DOCS/CHILD.ELF`参照）。parentが
//! `waitpid`でblockするため、childのstdoutとexit frameは必ずparentの
//! `waitpid verified`より先に出る＝確定的なframe列になる。
//! 失敗時は70で終了する。E2Eのfile-waitpid検査が使う。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{ECHILD, STDOUT, SyscallNumber};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。childもparentも42で終わる。
const SUCCESS_EXIT: u32 = 42;
/// 単一imageのmanifestではこのguestが最初のprocessなのでpid 0。
const SELF_PID: usize = 0;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
/// disk image fixtureの`DOCS/CHILD.ELF`。childはpid 1 + 41 = 42で終了する。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";
const MESSAGE: &[u8] = b"waitpid verified\n";

/// MiniOS ABIの`spawn`を呼ぶ。`a0`/`a1`がELF path。戻り値はchildの
/// pidか負のerrno。
fn sys_spawn(path: *const u8, path_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0はpath pointer兼戻り値、a1/a7は引数である。pathはU+R検証済みのstatic rangeである。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a7") SyscallNumber::Spawn as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`waitpid`を呼ぶ。`a0`が対象pid。戻り値はchildの終了
/// codeか負のerrno。対象がliveならこのecallはkernel内でblockされ、
/// 対象の終了後に再実行されてcodeを返す。
fn sys_waitpid(pid: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0はpid兼戻り値、a7はsyscall番号である。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") pid => returned,
            in("a7") SyscallNumber::Waitpid as usize,
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

/// `_start`から呼ばれるRust本体。`spawn`→`waitpid`の往復とerrno経路を
/// 確認して42で終了する。
extern "C" fn guest_main() -> ! {
    // CHILD.ELFを新processとして起動する。childはpid 1を採番し、
    // `spawn-child`をstdoutへ書いて42で終了する。
    if sys_spawn(CHILD_PATH.as_ptr(), CHILD_PATH.len()) != CHILD_PID as isize {
        sys_exit(FAILURE_EXIT);
    }

    // childはpid+41=42で終了する。parentはここでblockされ、childの
    // stdoutとexit frameが先に出てからcode 42を回収する。
    if sys_waitpid(CHILD_PID) != SUCCESS_EXIT as isize {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：reap済みのpid、自分自身、存在しないpidはECHILD。
    if sys_waitpid(CHILD_PID) != ECHILD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_waitpid(SELF_PID) != ECHILD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_waitpid(99) != ECHILD {
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
