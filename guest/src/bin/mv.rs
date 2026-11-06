//! user shellの`mv`（`BIN/MV.ELF`）。
//!
//! `mv <from> <to>`で、fileまたはdirectoryを別の名前や別のdirectoryへ移す。
//! 既存の`<to>`は`rename`の規約どおり置き換える。失敗はstderrへ1行出して
//! 1で、引数の数が違えば使い方を出して2で終了する。

#![no_std]
#![no_main]

use minios_guest::{Args, Errno, eprintln, fs::rename};

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut words = args.skip(1);
    let (Some(from), Some(to), None) = (words.next(), words.next(), words.next()) else {
        eprintln!("usage: mv <from> <to>");
        return 2;
    };
    match rename(from, to) {
        Ok(()) => 0,
        Err(Errno(errno)) => {
            let from = core::str::from_utf8(from).unwrap_or("?");
            let to = core::str::from_utf8(to).unwrap_or("?");
            eprintln!("mv: {from} -> {to}: cannot move (errno {errno})");
            1
        }
    }
}
