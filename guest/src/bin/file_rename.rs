//! MiniOS guestの`rename`サンプル。
//!
//! fileをrenameして開いているfdが有効なまま動くこと、旧名が`ENOENT`に
//! なること、置き換えられたfileを指すfdが`EBADF`で失効すること、
//! directoryや別dirへのrenameがerrnoを返すことを確認し、42で終了する。
//! 失敗時は70で終了する。E2Eのfile-rename検査が使う。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{
    EBADF, EINVAL, EISDIR, ENOENT, EXDEV, FIRST_FILE_FD, STDOUT, SyscallNumber,
};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// renameするfile名と内容。内容はguestが照合する。
const OLD_PATH: &[u8] = b"RENAME.TXT";
const NEW_PATH: &[u8] = b"RENAMED.TXT";
const VICTIM_PATH: &[u8] = b"VICTIM.TXT";
const DIR_PATH: &[u8] = b"DOCS";
const CROSS_PATH: &[u8] = b"DOCS/X.TXT";
const LFN_PATH: &[u8] = b"long name.txt";
const MISSING_PATH: &[u8] = b"MISSING.TXT";
const PAYLOAD: &[u8] = b"renamed by guest\n";
const MESSAGE: &[u8] = b"rename verified\n";
const BUFFER_LEN: usize = 64;

/// MiniOS ABIの`create`を呼ぶ。戻り値はwritable fdか負のerrno。
fn sys_create(path: *const u8, path_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0はpath pointer兼戻り値、a1/a7は引数である。pathはU+R検証済みのstatic rangeである。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a7") SyscallNumber::Create as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`open`を呼ぶ。戻り値はread-only fdか負のerrno。
fn sys_open(path: *const u8, path_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a7") SyscallNumber::Open as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`rename`を呼ぶ。`a0`/`a1`がsource path、`a2`/`a3`が
/// target path。戻り値は0か負のerrno。
fn sys_rename(old_ptr: *const u8, old_len: usize, new_ptr: *const u8, new_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0は戻り値、a0..a3/a7は引数である。両pathはU+R検証済みのstatic rangeである。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") old_ptr as usize => returned,
            in("a1") old_len,
            in("a2") new_ptr as usize,
            in("a3") new_len,
            in("a7") SyscallNumber::Rename as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`read`を呼ぶ。戻り値は読んだbyte数、EOFは0、負はerrno。
fn sys_read(fd: usize, pointer: *mut u8, len: usize) -> isize {
    let fd_argument = fd as isize;
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd_argument => returned,
            in("a1") pointer as usize,
            in("a2") len,
            in("a7") SyscallNumber::Read as usize,
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

/// MiniOS ABIの`close`を呼ぶ。戻り値は0か負のerrno。
fn sys_close(fd: usize) -> isize {
    let fd_argument = fd as isize;
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd_argument => returned,
            in("a7") SyscallNumber::Close as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`exit`。kernelはこの呼び出しの後guestへ戻らない。
fn sys_exit(code: u32) -> ! {
    // Safety: exitのecallはresumeしない契約のため、noreturnでよい。
    unsafe {
        asm!(
            "ecall",
            in("a0") code as isize,
            in("a7") SyscallNumber::Exit as usize,
            options(noreturn),
        );
    }
}

/// `_start`から呼ばれるRust本体。rename→fd継続→置き換え→errnoの契約を
/// 順に確かめる。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    // 作成して書き込み、閉じる。
    let fd = sys_create(OLD_PATH.as_ptr(), OLD_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 読み専用で開いたままrenameする。entryの位置は変わらないため、
    // 開いているfdはそのまま内容を読める。
    let fd = sys_open(OLD_PATH.as_ptr(), OLD_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_rename(
        OLD_PATH.as_ptr(),
        OLD_PATH.len(),
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    let mut buffer = [0u8; BUFFER_LEN];
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if buffer[..PAYLOAD.len()] != *PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 旧名はENOENT、新名で開ける。同名へのrenameは成功のno-op。
    if sys_open(OLD_PATH.as_ptr(), OLD_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：不在source、directoryのsource/target、別dir、非8.3名。
    if sys_rename(
        MISSING_PATH.as_ptr(),
        MISSING_PATH.len(),
        OLD_PATH.as_ptr(),
        OLD_PATH.len(),
    ) != ENOENT
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        DIR_PATH.as_ptr(),
        DIR_PATH.len(),
        OLD_PATH.as_ptr(),
        OLD_PATH.len(),
    ) != EISDIR
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        DIR_PATH.as_ptr(),
        DIR_PATH.len(),
    ) != EISDIR
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        CROSS_PATH.as_ptr(),
        CROSS_PATH.len(),
    ) != EXDEV
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        LFN_PATH.as_ptr(),
        LFN_PATH.len(),
    ) != EINVAL
    {
        sys_exit(FAILURE_EXIT);
    }

    // 既存fileへのrenameは置き換える。target側のfdは失効し、
    // target名で開くとsourceの内容が読める。
    let fd = sys_create(VICTIM_PATH.as_ptr(), VICTIM_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let victim = sys_open(VICTIM_PATH.as_ptr(), VICTIM_PATH.len());
    if victim < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let victim = victim as usize;
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        VICTIM_PATH.as_ptr(),
        VICTIM_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(victim, buffer.as_mut_ptr(), 8) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(NEW_PATH.as_ptr(), NEW_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(VICTIM_PATH.as_ptr(), VICTIM_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if buffer[..PAYLOAD.len()] != *PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
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
