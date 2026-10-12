//! proc-kill検査用guest。errno契約を確認した後、stdin待ちのvictim
//! 二つへkillを送り、正常終了する。
#![no_std]
#![no_main]

use minios_abi::syscall::{ECHILD, EINVAL};
use minios_guest::{
    Args, Errno, println,
    process::{getpid, kill},
};
minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // 自分自身へのkillは`exit`の役目としてEINVAL、不在pidはECHILD。
    assert_eq!(kill(getpid()), Err(Errno(EINVAL)));
    assert_eq!(kill(99), Err(Errno(ECHILD)));
    // stdin待ちで止まっているvictim二つへ終了要求。tableに残る間の
    // 二度目のkillもidempotentに0を返す。
    for pid in [1, 2] {
        assert_eq!(kill(pid), Ok(()));
        assert_eq!(kill(pid), Ok(()));
    }
    println!("kill syscalls verified");
    7
}
