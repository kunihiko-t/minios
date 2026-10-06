//! proc-fault検査のwaiter。二つの異常終了を一回ずつ回収し、正常終了する。
#![no_std]
#![no_main]

use minios_abi::syscall::ECHILD;
use minios_guest::{Args, Errno, println, process::wait};
minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    for pid in [1, 2] {
        assert_eq!(wait(pid), Ok(70));
        assert_eq!(wait(pid), Err(Errno(ECHILD)));
    }
    println!("fault waitpid verified");
    42
}
