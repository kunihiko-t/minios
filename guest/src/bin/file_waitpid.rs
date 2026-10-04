//! MiniOS guestの`waitpid`サンプル。
//!
//! `spawn`で`DOCS/CHILD.ELF`を起動し、`waitpid`がchildの終了code 42を
//! 返すこと、reap済みpid・自分自身・存在しないpidが`ECHILD`を返すことを
//! 確認して42で終了する。childは`spawn-child`をstdoutへ書き`pid + 41` =
//! 42で終了する（disk image fixtureの`DOCS/CHILD.ELF`参照）。parentが
//! `waitpid`でblockするため、childのstdoutとexit frameは必ずparentの
//! `waitpid verified`より先に出る＝確定的なframe列になる。
//! 失敗時はpanicし、70で終了する。E2Eのfile-waitpid検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::ECHILD;
use minios_guest::{
    Args, Errno, println,
    process::{spawn, wait},
};

/// 正常終了code。childもparentも42で終わる。
const SUCCESS_EXIT: i32 = 42;
/// 単一imageのmanifestではこのguestが最初のprocessなのでpid 0。
const SELF_PID: usize = 0;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
/// disk image fixtureの`DOCS/CHILD.ELF`。childはpid 1 + 41 = 42で終了する。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // CHILD.ELFを新processとして起動する。childはpid 1を採番し、
    // `spawn-child`をstdoutへ書いて42で終了する。
    assert_eq!(spawn(CHILD_PATH, &[]), Ok(CHILD_PID));

    // childはpid+41=42で終了する。parentはここでblockされ、childの
    // stdoutとexit frameが先に出てからcode 42を回収する。
    assert_eq!(wait(CHILD_PID), Ok(SUCCESS_EXIT));

    // errno契約：reap済みのpid、自分自身、存在しないpidはECHILD。
    assert_eq!(wait(CHILD_PID), Err(Errno(ECHILD)));
    assert_eq!(wait(SELF_PID), Err(Errno(ECHILD)));
    assert_eq!(wait(99), Err(Errno(ECHILD)));

    println!("waitpid verified");
    SUCCESS_EXIT
}
