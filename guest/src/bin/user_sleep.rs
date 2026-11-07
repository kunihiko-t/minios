//! MiniOS guestの`clock`/`sleep`/`yield`/`realtime`サンプル。
//!
//! `clock`を読み、`sleep(50)`の後にもう一度読んで差が50 ms以上であることを
//! 確かめる。`yield`と`sleep(0)`がどちらも0を返すことも確かめる。
//! 実時刻は、QEMU検査が`-rtc base=2001-02-03T04:05:06`で始めたRTCの起点から
//! 10分以内にあり、UTCの暦で2001-02-03の4時台であること、`sleep`の前後で
//! 50 ms以上進むことを確かめ、42で終了する。
//! 失敗時はpanicし、70で終了する。測った差は実行ごとに揺れるため出力しない。
//! E2Eのuser-sleep検査が使う。

#![no_std]
#![no_main]

use minios_guest::{
    Args, println,
    process::clock_ms,
    sys::{sys_sleep, sys_yield},
    time::{DateTime, realtime_ms},
};

const SLEEP_MILLIS: usize = 50;
/// QEMU検査が`-rtc base=2001-02-03T04:05:06`で渡すRTCの起点（Unix millisecond）。
const RTC_BASE_MS: u64 = 981_173_106_000;
/// 起動からこの検査までに経ってよい時間の上限。
const RTC_WINDOW_MS: u64 = 10 * 60 * 1_000;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let real_before = realtime_ms().expect("QEMU virt has a goldfish RTC");
    assert!((RTC_BASE_MS..RTC_BASE_MS + RTC_WINDOW_MS).contains(&real_before));
    let date = DateTime::from_unix_seconds(real_before / 1_000);
    assert_eq!(
        (date.year, date.month, date.day, date.hour),
        (2001, 2, 3, 4)
    );

    // 戻り値0の契約も確かめるため、`sleep`と`yield`は生のsyscallで呼ぶ。
    let before = clock_ms();
    assert_eq!(sys_sleep(SLEEP_MILLIS), 0);
    let after = clock_ms();
    assert!(after >= before + SLEEP_MILLIS as u64);
    assert!(sys_yield() == 0 && sys_sleep(0) == 0);

    let real_after = realtime_ms().expect("QEMU virt has a goldfish RTC");
    assert!(real_after >= real_before + SLEEP_MILLIS as u64);
    println!("sleep verified");
    42
}
