//! MiniOS guestの`readdir`サンプル。
//!
//! `readdir`でrootの3件（HELLO.TXT→file、DOCS→dir、`Long File
//! Name.txt`→file）をindex順に、`DOCS`の4件（NOTE.TXT、CHILD.ELF、
//! FDCHILD.ELF、PIPECH.ELF）をindex順に
//! 確認し、index超過の0・file pathの`ENOTDIR`・不在pathの`ENOENT`・
//! 書けないout pointerの`EFAULT`を確かめて42で終了する。
//! 失敗時はpanicし、70で終了する。E2Eのfile-readdir検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EFAULT, ENOENT, ENOTDIR, STAT_KIND_DIR, STAT_KIND_FILE};
use minios_guest::{Args, Errno, fs::read_dir, println, sys::sys_readdir};

/// disk image fixtureの`DOCS`directory。
const DOCS_PATH: &[u8] = b"DOCS";
/// directoryではないfile path。`ENOTDIR`を返す。
const FILE_PATH: &[u8] = b"HELLO.TXT";
const MISSING_PATH: &[u8] = b"MISSING";

/// `path`のentryがindex順に`expected`と一致し、その次で末尾になることを
/// 確かめる。末尾の確認で`readdir`はindex超過の0を返す。
fn assert_entries(path: &[u8], expected: &[(&[u8], u32)]) {
    let mut entries = read_dir(path);
    for (name, kind) in expected {
        let entry = entries.next().unwrap().unwrap();
        assert_eq!(&entry.name[..entry.name_len as usize], *name);
        assert_eq!(entry.kind, *kind);
    }
    assert!(entries.next().is_none());
}

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // 空pathはroot directory。index順に3件、以降は末尾超過の0。
    assert_entries(
        b"",
        &[
            (b"HELLO.TXT", STAT_KIND_FILE),
            (b"DOCS", STAT_KIND_DIR),
            (b"Long File Name.txt", STAT_KIND_FILE),
        ],
    );

    // subdirectoryも同じindex規約。`.`/`..`は列挙に含まれない。
    assert_entries(
        DOCS_PATH,
        &[
            (b"NOTE.TXT", STAT_KIND_FILE),
            (b"CHILD.ELF", STAT_KIND_FILE),
            (b"FDCHILD.ELF", STAT_KIND_FILE),
            (b"PIPECH.ELF", STAT_KIND_FILE),
        ],
    );

    // errno契約：fileへのreaddirはENOTDIR、不在pathはENOENT、
    // 書けないout pointerはEFAULT（sourceのside effectより先に確定）。
    assert_eq!(read_dir(FILE_PATH).next(), Some(Err(Errno(ENOTDIR))));
    assert_eq!(read_dir(MISSING_PATH).next(), Some(Err(Errno(ENOENT))));
    assert_eq!(
        sys_readdir(
            DOCS_PATH.as_ptr(),
            DOCS_PATH.len(),
            0,
            core::ptr::null_mut()
        ),
        EFAULT
    );

    println!("readdir verified");
    42
}
