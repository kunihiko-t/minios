//! MiniOS scheduler検証用のguest。
//!
//! "a1\n" → yield → "a2\n" → yield → "a3\n" → yield → exit(0) と動く。
//! 最初のyieldで同居するsched_bへ順番が回るため、b列の出力がa1とa3の間へ
//! 挟まる。harnessはその交差をもって実際の切り替えを確認する。
//! 以前はbusy-waitでtimerプリエンプションを待っていたが、それではtime slice
//! の長さとQEMUの速度に結果が左右される。yieldなら切り替えの時点が
//! guestの命令列で決まる。

#![no_std]
#![no_main]

use minios_guest::{Args, print, process::yield_now};

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    for marker in ["a1\n", "a2\n", "a3\n"] {
        print!("{marker}");
        yield_now();
    }
    0
}
