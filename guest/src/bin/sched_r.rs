//! MiniOS scheduler検証用のstdin待ちguest。
//!
//! "r1\n" を出してから `read(stdin)` でblockする。入力未到着の間は
//! このprocessが選ばれず、同居するsched_a/spinとsched_b/quickが進み続ける。
//! hostがStdin frameを送るとreadが完了して "r2\n" を出してexit(5)する。

#![no_std]
#![no_main]

use minios_abi::syscall::STDIN;
use minios_guest::{Args, io, print};

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    print!("r1\n");
    let mut buffer = [0u8; 16];
    let _ = io::read(STDIN, &mut buffer);
    print!("r2\n");
    5
}
