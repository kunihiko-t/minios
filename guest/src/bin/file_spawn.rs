//! MiniOS guestの`spawn`/`getpid`サンプル。
//!
//! `getpid`が自分のpidを返すこと、`spawn`が`DOCS/CHILD.ELF`を新process
//! として起動してchildのpidを返すこと、不在pathが`ENOENT`、directoryが
//! `EISDIR`、非ELF fileが`EINVAL`を返すことを確認し、42で終了する。
//! childは起動されると`spawn-child`をstdoutへ書き`pid + 41` = 42で
//! 終了する（disk image fixtureの`DOCS/CHILD.ELF`参照）。失敗時はpanicし、
//! 70で終了する。E2Eのfile-spawn検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EINVAL, EISDIR, ENOENT};
use minios_guest::{
    Args, Errno, println,
    process::{getpid, spawn},
};

/// 単一imageのmanifestではこのguestが最初のprocessなのでpid 0。
const SELF_PID: usize = 0;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
/// disk image fixtureの`DOCS/CHILD.ELF`。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";
const DIR_PATH: &[u8] = b"DOCS";
const NOTELF_PATH: &[u8] = b"HELLO.TXT";
const MISSING_PATH: &[u8] = b"MISSING.ELF";

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // getpidはtable採番のpidを返す。単一image manifestではpid 0。
    assert_eq!(getpid(), SELF_PID);

    // CHILD.ELFを新processとして起動する。childはpid 1を採番し、
    // 起動後に`spawn-child`をstdoutへ書いて42で終了する。
    assert_eq!(spawn(CHILD_PATH, &[]), Ok(CHILD_PID));

    // errno契約：不在pathはENOENT、directoryはEISDIR、非ELF fileは
    // EINVAL。空pathはdispatch段階でEINVAL。
    assert_eq!(spawn(MISSING_PATH, &[]), Err(Errno(ENOENT)));
    assert_eq!(spawn(DIR_PATH, &[]), Err(Errno(EISDIR)));
    assert_eq!(spawn(NOTELF_PATH, &[]), Err(Errno(EINVAL)));
    assert_eq!(spawn(b"", &[]), Err(Errno(EINVAL)));

    println!("spawn verified");
    42
}
