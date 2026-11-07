//! MiniOS scheduler検証用の短命guest。
//!
//! "b1\n".."b3\n" をすぐに出力してexit(7)する。sched_aがa1の後に
//! yieldすると順番が回ってきて、b列の出力がa列の途中に現れる。

#![no_std]
#![no_main]

use minios_guest::{Args, print};

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    for marker in ["b1\n", "b2\n", "b3\n"] {
        print!("{marker}");
    }
    7
}
