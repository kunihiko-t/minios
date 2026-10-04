//! MiniOS guestの`stat`/`fstat`サンプル。
//!
//! `stat`で`DOCS/NOTE.TXT`（file, 17 byte）、`DOCS/CHILD.ELF`（file,
//! 204 byte）、`DOCS`（directory, size 0）のmetadataを確認し、不在pathの
//! `ENOENT`・file途中要素の`ENOTDIR`・書けないout pointerの`EFAULT`を
//! 確かめる。`open`したfdへの`fstat`が`stat`と同じmetadataを返すことと、
//! stdoutへの`fstat`がsize 0の`STAT_KIND_CONSOLE`を返すことも確認して
//! 42で終了する。
//! 失敗時は70で終了する。E2Eのfile-stat検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{
    EFAULT, ENOENT, ENOTDIR, FIRST_FILE_FD, STAT_KIND_CONSOLE, STAT_KIND_DIR, STAT_KIND_FILE,
    STAT_LEN, STDOUT, Stat,
};
use minios_guest::sys::{sys_exit, sys_fstat, sys_open, sys_stat, sys_write};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// disk image fixtureの`DOCS/NOTE.TXT`（"note inside docs\n"、17 byte）。
const NOTE_PATH: &[u8] = b"DOCS/NOTE.TXT";
/// disk image fixtureの`DOCS/CHILD.ELF`（最小ELF64、204 byte）。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";
/// disk image fixtureの`DOCS`directory。
const DOCS_PATH: &[u8] = b"DOCS";
/// file要素を途中に挟む不正path。最終要素の解決前に`ENOTDIR`を返す。
const THROUGH_FILE_PATH: &[u8] = b"DOCS/NOTE.TXT/DEEP";
const MISSING_PATH: &[u8] = b"MISSING.TXT";
const MESSAGE: &[u8] = b"stat verified\n";

/// `path`の`stat`を呼び、成功なら`Stat`を返す。失敗時はerrnoを返す。
fn stat_of(path: &[u8]) -> Result<Stat, isize> {
    let mut out = [0u8; STAT_LEN];
    let ret = sys_stat(path.as_ptr(), path.len(), out.as_mut_ptr());
    if ret < 0 {
        return Err(ret);
    }
    if ret != STAT_LEN as isize {
        return Err(isize::MIN);
    }
    Ok(Stat::from_le_bytes(out))
}

/// `_start`から呼ばれるRust本体。stat/fstatの契約を順に確かめる。
extern "C" fn guest_main() -> ! {
    // file pathのstat：sizeとkindがdir entryと一致すること。
    let note = match stat_of(NOTE_PATH) {
        Ok(stat) => stat,
        Err(_) => sys_exit(FAILURE_EXIT),
    };
    if note.size != 17 || note.kind != STAT_KIND_FILE {
        sys_exit(FAILURE_EXIT);
    }
    let child = match stat_of(CHILD_PATH) {
        Ok(stat) => stat,
        Err(_) => sys_exit(FAILURE_EXIT),
    };
    if child.size != 204 || child.kind != STAT_KIND_FILE {
        sys_exit(FAILURE_EXIT);
    }

    // directory pathのstat：kind=dir、sizeはFAT32の規約で0。
    let docs = match stat_of(DOCS_PATH) {
        Ok(stat) => stat,
        Err(_) => sys_exit(FAILURE_EXIT),
    };
    if docs.kind != STAT_KIND_DIR {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：不在pathはENOENT、fileを途中要素に持つpathはENOTDIR、
    // 書けないout pointerはEFAULT（sourceのside effectより先に確定する）。
    if stat_of(MISSING_PATH) != Err(ENOENT) {
        sys_exit(FAILURE_EXIT);
    }
    if stat_of(THROUGH_FILE_PATH) != Err(ENOTDIR) {
        sys_exit(FAILURE_EXIT);
    }
    if sys_stat(NOTE_PATH.as_ptr(), NOTE_PATH.len(), core::ptr::null_mut()) != EFAULT {
        sys_exit(FAILURE_EXIT);
    }

    // open済みfdへのfstatはstatと同じmetadataを返す。stdoutはconsole。
    let fd = sys_open(NOTE_PATH.as_ptr(), NOTE_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let mut out = [0u8; STAT_LEN];
    if sys_fstat(fd as usize, out.as_mut_ptr()) != STAT_LEN as isize {
        sys_exit(FAILURE_EXIT);
    }
    let opened = Stat::from_le_bytes(out);
    if opened.size != 17 || opened.kind != STAT_KIND_FILE {
        sys_exit(FAILURE_EXIT);
    }
    if sys_fstat(STDOUT, out.as_mut_ptr()) != STAT_LEN as isize {
        sys_exit(FAILURE_EXIT);
    }
    let console = Stat::from_le_bytes(out);
    if console.size != 0 || console.kind != STAT_KIND_CONSOLE {
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
