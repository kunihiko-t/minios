//! MiniOS guestの`stat`/`fstat`サンプル。
//!
//! `stat`で`DOCS/NOTE.TXT`（file, 17 byte）、`DOCS/CHILD.ELF`（file,
//! 204 byte）、`DOCS`（directory, size 0）のmetadataを確認し、不在pathの
//! `ENOENT`・file途中要素の`ENOTDIR`・書けないout pointerの`EFAULT`を
//! 確かめる。`open`したfdへの`fstat`が`stat`と同じmetadataを返すことと、
//! stdoutへの`fstat`がsize 0の`STAT_KIND_CONSOLE`を返すことも確認して
//! 42で終了する。
//! 失敗時はpanicし、70で終了する。E2Eのfile-stat検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{
    EFAULT, ENOENT, ENOTDIR, STAT_KIND_CONSOLE, STAT_KIND_DIR, STAT_KIND_FILE, STAT_LEN, STDOUT,
    Stat,
};
use minios_guest::{
    Args, Errno,
    fs::{File, stat},
    println,
    sys::{sys_fstat, sys_stat},
};

/// disk image fixtureの`DOCS/NOTE.TXT`（"note inside docs\n"、17 byte）。
const NOTE_PATH: &[u8] = b"DOCS/NOTE.TXT";
/// disk image fixtureの`DOCS/CHILD.ELF`（最小ELF64、204 byte）。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";
/// disk image fixtureの`DOCS`directory。
const DOCS_PATH: &[u8] = b"DOCS";
/// file要素を途中に挟む不正path。最終要素の解決前に`ENOTDIR`を返す。
const THROUGH_FILE_PATH: &[u8] = b"DOCS/NOTE.TXT/DEEP";
const MISSING_PATH: &[u8] = b"MISSING.TXT";

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let file = |size| Stat {
        size,
        kind: STAT_KIND_FILE,
    };

    // file pathのstat：sizeとkindがdir entryと一致すること。
    assert_eq!(stat(NOTE_PATH), Ok(file(17)));
    assert_eq!(stat(CHILD_PATH), Ok(file(204)));

    // directory pathのstat：kind=dir、sizeはFAT32の規約で0。
    assert_eq!(stat(DOCS_PATH).unwrap().kind, STAT_KIND_DIR);

    // errno契約：不在pathはENOENT、fileを途中要素に持つpathはENOTDIR、
    // 書けないout pointerはEFAULT（sourceのside effectより先に確定する）。
    assert_eq!(stat(MISSING_PATH), Err(Errno(ENOENT)));
    assert_eq!(stat(THROUGH_FILE_PATH), Err(Errno(ENOTDIR)));
    assert_eq!(
        sys_stat(NOTE_PATH.as_ptr(), NOTE_PATH.len(), core::ptr::null_mut()),
        EFAULT
    );

    // open済みfdへのfstatはstatと同じmetadataを返す。stdoutはconsoleで、
    // dropで閉じないよう`File`へ包まず生のfdで読む。
    let note = File::open(NOTE_PATH).unwrap();
    assert_eq!(note.stat(), Ok(file(17)));
    let mut out = [0u8; STAT_LEN];
    assert_eq!(sys_fstat(STDOUT, out.as_mut_ptr()), STAT_LEN as isize);
    assert_eq!(
        Stat::from_le_bytes(out),
        Stat {
            size: 0,
            kind: STAT_KIND_CONSOLE
        }
    );

    println!("stat verified");
    42
}
