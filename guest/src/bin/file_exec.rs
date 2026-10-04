//! MiniOS guestの`exec`サンプル。
//!
//! `exec`のerrno経路（不在pathの`ENOENT`、directoryの`EISDIR`、
//! 非ELFの`EINVAL`、読めないpath pointerの`EFAULT`）を確かめてから
//! `DOCS/CHILD.ELF`へexecする。成功時はこのimageが置き換わるため
//! 戻らず、CHILD.ELFが`spawn-child`をstdoutへ出して`getpid()+41`で
//! 終了する。exit code 41はexecがpid 0を保持したことの証明である。
//! errno検査のどれかが失敗すれば70で終了する。E2Eのfile-exec検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{EFAULT, EINVAL, EISDIR, ENOENT};
use minios_guest::sys::{sys_exec, sys_exit};

/// exit異常の的内code。errno契約違反やpanicで使う。
const FAILURE_EXIT: u32 = 70;
/// directoryを指すpath。`exec`は`EISDIR`を返す。
const DIR_PATH: &[u8] = b"DOCS";
/// ELFではないfile path。`exec`は`EINVAL`を返す。
const TEXT_PATH: &[u8] = b"HELLO.TXT";
/// 存在しないpath。`exec`は`ENOENT`を返す。
const MISSING_PATH: &[u8] = b"MISSING";
/// exec対象の子image。disk image fixtureに置いた最小ELF。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";

/// `_start`から呼ばれるRust本体。errno契約を順に確かめてからexecする。
extern "C" fn guest_main() -> ! {
    // errno契約：不在はENOENT、directoryはEISDIR、非ELFはEINVAL。
    if sys_exec(MISSING_PATH.as_ptr(), MISSING_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_exec(DIR_PATH.as_ptr(), DIR_PATH.len()) != EISDIR {
        sys_exit(FAILURE_EXIT);
    }
    if sys_exec(TEXT_PATH.as_ptr(), TEXT_PATH.len()) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }
    // 読めないpath pointerはEFAULT、長さ0はEINVAL。
    if sys_exec(core::ptr::null(), 4) != EFAULT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_exec(CHILD_PATH.as_ptr(), 0) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }

    // 成功したexecは戻らない。戻ったら失敗である。
    let returned = sys_exec(CHILD_PATH.as_ptr(), CHILD_PATH.len());
    let _ = returned;
    sys_exit(FAILURE_EXIT);
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
