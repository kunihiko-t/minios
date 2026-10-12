//! proc-kill検査のwaiter。killされた二つの終了codeを一回ずつ回収し、
//! 正常終了する。
#![no_std]
#![no_main]

use minios_abi::syscall::{ECHILD, EXIT_CODE_KILLED};
use minios_guest::{Args, Errno, println, process::wait};
minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    for pid in [1, 2] {
        assert_eq!(wait(pid), Ok(EXIT_CODE_KILLED as i32));
        assert_eq!(wait(pid), Err(Errno(ECHILD)));
    }
    println!("kill waitpid verified");
    42
}
