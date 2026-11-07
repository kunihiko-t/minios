//! user shellの`sleep`（`BIN/SLEEP.ELF`）。
//!
//! `sleep <seconds>`で、指定した秒数だけ待ってから0で終了する。秒数は
//! `2`のような整数か、`0.25`のように小数点以下3桁までの小数で書ける。
//! 待つ間はkernelの`sleep`でblockするため、他のprocessが走る。引数が
//! 無いか読めなければ使い方を出して2で終了する。

#![no_std]
#![no_main]

use minios_guest::{Args, eprintln, process::sleep_ms};

/// `seconds`をmillisecondへ直す。小数点以下は3桁まで受け付ける。
fn parse_millis(seconds: &[u8]) -> Option<usize> {
    let (whole, fraction) = match seconds.iter().position(|&byte| byte == b'.') {
        Some(dot) => (&seconds[..dot], &seconds[dot + 1..]),
        None => (seconds, &[][..]),
    };
    if (whole.is_empty() && fraction.is_empty()) || fraction.len() > 3 {
        return None;
    }
    let digits = |part: &[u8]| {
        part.iter().try_fold(0usize, |value, &byte| {
            if !byte.is_ascii_digit() {
                return None;
            }
            value.checked_mul(10)?.checked_add(usize::from(byte - b'0'))
        })
    };
    let mut millis = digits(whole)?.checked_mul(1_000)?;
    let mut scale = 100;
    for &byte in fraction {
        if !byte.is_ascii_digit() {
            return None;
        }
        millis = millis.checked_add(usize::from(byte - b'0') * scale)?;
        scale /= 10;
    }
    Some(millis)
}

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut words = args.skip(1);
    let (Some(seconds), None) = (words.next(), words.next()) else {
        eprintln!("usage: sleep <seconds>");
        return 2;
    };
    let Some(millis) = parse_millis(seconds) else {
        eprintln!("usage: sleep <seconds>");
        return 2;
    };
    sleep_ms(millis);
    0
}
