//! MiniOS guestの`exec`サンプル。
//!
//! `exec`のerrno経路（不在pathの`ENOENT`、directoryの`EISDIR`、
//! 非ELFの`EINVAL`、読めないpath pointerの`EFAULT`）を確かめてから
//! `DOCS/CHILD.ELF`へexecする。成功時はこのimageが置き換わるため
//! 戻らず、CHILD.ELFが`spawn-child`をstdoutへ出して`getpid()+41`で
//! 終了する。exit code 41はexecがpid 0を保持したことの証明である。
//! errno検査のどれかが失敗すればpanicし、70で終了する。
//! E2Eのfile-exec検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EFAULT, EINVAL, EISDIR, ENOENT};
use minios_guest::{Args, Errno, FAILURE_EXIT, process::exec, sys::sys_exec};

/// directoryを指すpath。`exec`は`EISDIR`を返す。
const DIR_PATH: &[u8] = b"DOCS";
/// ELFではないfile path。`exec`は`EINVAL`を返す。
const TEXT_PATH: &[u8] = b"HELLO.TXT";
/// 存在しないpath。`exec`は`ENOENT`を返す。
const MISSING_PATH: &[u8] = b"MISSING";
/// exec対象の子image。disk image fixtureに置いた最小ELF。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // errno契約：不在はENOENT、directoryはEISDIR、非ELFはEINVAL。
    assert_eq!(exec(MISSING_PATH), Errno(ENOENT));
    assert_eq!(exec(DIR_PATH), Errno(EISDIR));
    assert_eq!(exec(TEXT_PATH), Errno(EINVAL));
    // 読めないpath pointerはEFAULT、長さ0はEINVAL。
    assert_eq!(sys_exec(core::ptr::null(), 4), EFAULT);
    assert_eq!(exec(b""), Errno(EINVAL));

    // 成功したexecは戻らない。戻ったら失敗である。
    let _ = exec(CHILD_PATH);
    FAILURE_EXIT
}
