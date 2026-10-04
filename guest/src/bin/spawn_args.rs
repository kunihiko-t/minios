//! MiniOS guestの`spawn` argvサンプル。
//!
//! disk image fixtureの`DOCS/ECHO.ELF`（argvを順にstdoutへ書いて42で
//! 終了する`minios-guest`）を起動する。先にargvの誤りを確かめる:
//! argcの上限超過と長すぎる文字列は`EINVAL`、読めないargv列や文字列は
//! `EFAULT`で、どれもchildを作らない。続いてargc=0で起動してargvが
//! basenameの`ECHO.ELF`だけであることを、argc=3で
//! `echoargs`、`alpha`、`beta gamma`がそのまま届くことを示す。
//! 失敗したspawnがpidを消費していればchildのpidがずれ、panicして70で終了する。
//! 最後に`spawn args verified`を出して42で終了する。E2Eのspawn-args検査が使う。

#![no_std]
#![no_main]

use minios_abi::{
    manifest::ARG_MAX_LEN,
    syscall::{EFAULT, EINVAL, SPAWN_MAX_ARGC},
};
use minios_guest::{
    Args, Errno, println,
    process::{spawn, wait},
    sys::sys_spawn,
};

const SUCCESS_EXIT: i32 = 42;
const ECHO_PATH: &[u8] = b"DOCS/ECHO.ELF";
/// 上限を1 byte超える文字列。
static LONG: [u8; ARG_MAX_LEN + 1] = [b'a'; ARG_MAX_LEN + 1];

/// argv entry列を生のまま`spawn`へ渡す。libraryの`spawn`はargc超過を
/// syscall前に拒否するため、kernel側の検査は生のsyscallで確かめる。
fn spawn_raw(argv: &[[u64; 2]]) -> isize {
    sys_spawn(
        ECHO_PATH.as_ptr(),
        ECHO_PATH.len(),
        argv.as_ptr(),
        argv.len(),
    )
}

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // どの誤りもchildを作らず、pidも消費しない。
    let x = [b"x".as_ptr() as u64, 1];
    assert_eq!(spawn_raw(&[x; SPAWN_MAX_ARGC + 1]), EINVAL);
    assert_eq!(spawn(ECHO_PATH, &[b"echoargs", &LONG]), Err(Errno(EINVAL)));
    assert_eq!(
        sys_spawn(ECHO_PATH.as_ptr(), ECHO_PATH.len(), core::ptr::null(), 1),
        EFAULT
    );
    let echoargs = [b"echoargs".as_ptr() as u64, 8];
    assert_eq!(spawn_raw(&[echoargs, [0, 1]]), EFAULT);

    // argc=0は従来どおりbasenameだけがargv[0]になる。
    assert_eq!(spawn(ECHO_PATH, &[]), Ok(1));
    assert_eq!(wait(1), Ok(SUCCESS_EXIT));

    assert_eq!(
        spawn(ECHO_PATH, &[b"echoargs", b"alpha", b"beta gamma"]),
        Ok(2)
    );
    assert_eq!(wait(2), Ok(SUCCESS_EXIT));

    println!("spawn args verified");
    SUCCESS_EXIT
}
