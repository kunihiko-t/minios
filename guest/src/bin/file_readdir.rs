//! MiniOS guestの`readdir`サンプル。
//!
//! `readdir`でrootの3件（HELLO.TXT→file、DOCS→dir、`Long File
//! Name.txt`→file）をindex順に、`DOCS`の2件（NOTE.TXT、CHILD.ELF）を
//! 確認し、index超過の0・file pathの`ENOTDIR`・不在pathの`ENOENT`・
//! 書けないout pointerの`EFAULT`を確かめて42で終了する。
//! 失敗時は70で終了する。E2Eのfile-readdir検査が使う。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{
    DIRENT_LEN, DirEnt, EFAULT, ENOENT, ENOTDIR, STAT_KIND_DIR, STAT_KIND_FILE, STDOUT,
    SyscallNumber,
};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// disk image fixtureの`DOCS`directory。
const DOCS_PATH: &[u8] = b"DOCS";
/// directoryではないfile path。`ENOTDIR`を返す。
const FILE_PATH: &[u8] = b"HELLO.TXT";
const MISSING_PATH: &[u8] = b"MISSING";
const MESSAGE: &[u8] = b"readdir verified\n";

/// MiniOS ABIの`readdir`を呼ぶ。`a0`/`a1`がpath（`a1`=0はroot）、
/// `a2`がindex、`a3`が`DirEnt`の書き込み先。戻り値は書いたbyte数
/// （`DIRENT_LEN`）、末尾超過は0、負はerrno。
fn sys_readdir(path: *const u8, path_len: usize, index: usize, out: *mut u8) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0はpath pointer兼戻り値、a1/a2/a3/a7は引数である。outはU+W検証対象である。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a2") index,
            in("a3") out as usize,
            in("a7") SyscallNumber::Readdir as usize,
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

/// `path`の`index`番目のentryを読む。`Ok`は`DirEnt`、`Err(0)`は
/// 末尾超過、それ以外の`Err`はerrno。
fn entry_at(path: &[u8], index: usize) -> Result<DirEnt, isize> {
    let mut out = [0u8; DIRENT_LEN];
    let ret = sys_readdir(path.as_ptr(), path.len(), index, out.as_mut_ptr());
    if ret == 0 {
        return Err(0);
    }
    if ret < 0 {
        return Err(ret);
    }
    if ret != DIRENT_LEN as isize {
        return Err(isize::MIN);
    }
    Ok(DirEnt::from_le_bytes(out))
}

/// `entry`のnameが`expected`と一致し`kind`も一致するか検査する。
fn entry_matches(entry: &DirEnt, expected: &[u8], kind: u32) -> bool {
    entry.name_len as usize == expected.len()
        && &entry.name[..expected.len()] == expected
        && entry.kind == kind
}

/// `_start`から呼ばれるRust本体。readdirの契約を順に確かめる。
extern "C" fn guest_main() -> ! {
    // 空pathはroot directory。index順に3件、以降は末尾超過の0。
    let expected_root: [(&[u8], u32); 3] = [
        (b"HELLO.TXT", STAT_KIND_FILE),
        (b"DOCS", STAT_KIND_DIR),
        (b"Long File Name.txt", STAT_KIND_FILE),
    ];
    for (index, (name, kind)) in expected_root.iter().enumerate() {
        match entry_at(b"", index) {
            Ok(entry) if entry_matches(&entry, name, *kind) => {}
            _ => sys_exit(FAILURE_EXIT),
        }
    }
    if entry_at(b"", 3) != Err(0) {
        sys_exit(FAILURE_EXIT);
    }

    // subdirectoryも同じindex規約。`.`/`..`は列挙に含まれない。
    let expected_docs: [(&[u8], u32); 2] = [
        (b"NOTE.TXT", STAT_KIND_FILE),
        (b"CHILD.ELF", STAT_KIND_FILE),
    ];
    for (index, (name, kind)) in expected_docs.iter().enumerate() {
        match entry_at(DOCS_PATH, index) {
            Ok(entry) if entry_matches(&entry, name, *kind) => {}
            _ => sys_exit(FAILURE_EXIT),
        }
    }
    if entry_at(DOCS_PATH, 2) != Err(0) {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：fileへのreaddirはENOTDIR、不在pathはENOENT、
    // 書けないout pointerはEFAULT（sourceのside effectより先に確定）。
    if entry_at(FILE_PATH, 0) != Err(ENOTDIR) {
        sys_exit(FAILURE_EXIT);
    }
    if entry_at(MISSING_PATH, 0) != Err(ENOENT) {
        sys_exit(FAILURE_EXIT);
    }
    if sys_readdir(
        DOCS_PATH.as_ptr(),
        DOCS_PATH.len(),
        0,
        core::ptr::null_mut(),
    ) != EFAULT
    {
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
