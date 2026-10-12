//! proc-fault検査用guest。argvに従い、不正命令か未写像storeを実行する。
#![no_std]
#![no_main]

use minios_guest::Args;
minios_guest::entry!(main);

fn main(mut args: Args) -> i32 {
    // 意図したU-mode例外だけを発生させ、kernelのタスク分離を検査する。
    // Safety: このguestはQEMU内の故障注入fixtureであり、例外後は再開されない。
    unsafe {
        match args.nth(1) {
            Some(b"illegal") => core::arch::asm!(".word 0", options(noreturn)),
            Some(b"store") => core::arch::asm!("sd zero, 0(zero)", options(noreturn)),
            _ => panic!("unknown fault fixture"),
        }
    }
}
