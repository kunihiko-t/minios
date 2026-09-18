//! MiniOS guestの`spawn`/`getpid`サンプル。
//!
//! `getpid`が自分のpidを返すこと、`spawn`が`DOCS/CHILD.ELF`を新process
//! として起動してchildのpidを返すこと、不在pathが`ENOENT`、directoryが
//! `EISDIR`、非ELF fileが`EINVAL`を返すことを確認し、42で終了する。
//! childは起動されると`spawn-child`をstdoutへ書き`pid + 41` = 42で
//! 終了する（disk image fixtureの`DOCS/CHILD.ELF`参照）。失敗時は70で
//! 終了する。E2Eのfile-spawn検査が使う。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{EINVAL, EISDIR, ENOENT, STDOUT, SyscallNumber};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// 単一imageのmanifestではこのguestが最初のprocessなのでpid 0。
const SELF_PID: isize = 0;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: isize = 1;
/// disk image fixtureの`DOCS/CHILD.ELF`。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";
const DIR_PATH: &[u8] = b"DOCS";
const NOTELF_PATH: &[u8] = b"HELLO.TXT";
const MISSING_PATH: &[u8] = b"MISSING.ELF";
const MESSAGE: &[u8] = b"spawn verified\n";

/// MiniOS ABIの`getpid`を呼ぶ。戻り値は自分のpid。
fn sys_getpid() -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0は戻り値、a7はsyscall番号である。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") 0usize => returned,
            in("a7") SyscallNumber::Getpid as usize,
            options(nostack),
        );
    }
    returned
}

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

/// `_start`から呼ばれるRust本体。`getpid`/`spawn`を確認して42で終了する。
extern "C" fn guest_main() -> ! {
    // getpidはtable採番のpidを返す。単一image manifestではpid 0。
    if sys_getpid() != SELF_PID {
        sys_exit(FAILURE_EXIT);
    }

    // CHILD.ELFを新processとして起動する。childはpid 1を採番し、
    // 起動後に`spawn-child`をstdoutへ書いて42で終了する。
    if sys_spawn(CHILD_PATH.as_ptr(), CHILD_PATH.len()) != CHILD_PID {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：不在pathはENOENT、directoryはEISDIR、非ELF fileは
    // EINVAL。空pathはdispatch段階でEINVAL。
    if sys_spawn(MISSING_PATH.as_ptr(), MISSING_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_spawn(DIR_PATH.as_ptr(), DIR_PATH.len()) != EISDIR {
        sys_exit(FAILURE_EXIT);
    }
    if sys_spawn(NOTELF_PATH.as_ptr(), NOTELF_PATH.len()) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }
    if sys_spawn(MISSING_PATH.as_ptr(), 0) != EINVAL {
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
