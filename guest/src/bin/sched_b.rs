//! MiniOS scheduler検証用の短命guest。
//!
//! 最初に一度yieldしてから "b1\n".."b3\n" を出力し、exit(7)する。
//! sched_aがa1の後にyieldすると順番が回ってきて、b列の出力がa列の途中に
//! 現れる。
//!
//! 先頭のyieldは、kernelがsched_aを最初に走らせた直後にtimer割り込みで
//! sched_bへ切り替えた場合の備えである。spawn中に期限を過ぎたtickは
//! 最初のdispatch直後に届き得るため、sched_bがa1より前に走ることがある。
//! そのときもsched_bは何も書かずに順番を返し、tickを再armされたsched_aが
//! a1を書いてからyieldする。

#![no_std]
#![no_main]

use minios_guest::{Args, print, process::yield_now};

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    yield_now();
    for marker in ["b1\n", "b2\n", "b3\n"] {
        print!("{marker}");
    }
    7
}
