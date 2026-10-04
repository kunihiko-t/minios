//! MiniOS guestの`clock`/`sleep`/`yield`サンプル。
//!
//! `clock`を読み、`sleep(50)`の後にもう一度読んで差が50 ms以上であることを
//! 確かめる。`yield`と`sleep(0)`がどちらも0を返すことも確かめ、42で終了する。
//! 失敗時はpanicし、70で終了する。測った差は実行ごとに揺れるため出力しない。
//! E2Eのuser-sleep検査が使う。

#![no_std]
#![no_main]

use minios_guest::{
    Args, println,
    process::clock_ms,
    sys::{sys_sleep, sys_yield},
};

const SLEEP_MILLIS: usize = 50;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // 戻り値0の契約も確かめるため、`sleep`と`yield`は生のsyscallで呼ぶ。
    let before = clock_ms();
    assert_eq!(sys_sleep(SLEEP_MILLIS), 0);
    let after = clock_ms();
    assert!(after >= before + SLEEP_MILLIS as u64);
    assert!(sys_yield() == 0 && sys_sleep(0) == 0);
    println!("sleep verified");
    42
}
