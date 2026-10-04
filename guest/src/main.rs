//! MiniOS guestの最小サンプル。
//!
//! カーネルは`a0=argc`、`a1=argv`、整列済み`sp`で起動する
//! (docs/reference/minicontainer-abi.mdの初期スタックABI)。
//! このprogramはprogram nameと引数を1個ずつ別の`write`でstdoutへ書き、
//! 42で終了する。`write`失敗とpanicは70で終了する。

#![no_std]
#![no_main]

use minios_abi::syscall::STDOUT;
use minios_guest::{Args, io};

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    for arg in args {
        io::write_all(STDOUT, arg).unwrap();
    }
    42
}
