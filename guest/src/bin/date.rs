//! user shellの`date`（`BIN/DATE.ELF`）。
//!
//! 現在のUTCの日時を`YYYY-MM-DD HH:MM:SS UTC`の形で1行出して0で終了する。
//! RTCの無いmachineなどで実時刻を読めなければ、stderrへ理由を1行出して
//! 1で終了する。引数は受け付けず、渡されれば使い方を出して2で終了する。

#![no_std]
#![no_main]

use minios_guest::{
    Args, Errno, eprintln, println,
    time::{DateTime, realtime_ms},
};

minios_guest::entry!(main);

fn main(mut args: Args) -> i32 {
    if args.nth(1).is_some() {
        eprintln!("usage: date");
        return 2;
    }
    match realtime_ms() {
        Ok(millis) => {
            println!("{} UTC", DateTime::from_unix_seconds(millis / 1_000));
            0
        }
        Err(Errno(errno)) => {
            eprintln!("date: cannot read the clock (errno {errno})");
            1
        }
    }
}
