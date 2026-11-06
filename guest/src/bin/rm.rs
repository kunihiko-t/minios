//! user shellの`rm`（`BIN/RM.ELF`）。
//!
//! 引数のpathを順に処理し、fileを消す（directoryは`rmdir`を使う）。失敗した引数はstderrへ1行出して
//! 次へ進み、最後に1で終了する。引数がなければ使い方を出して2で終了する。

#![no_std]
#![no_main]

use minios_guest::{Args, Errno, eprintln, fs::unlink};

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut code = 0;
    let mut handled = 0;
    for path in args.skip(1) {
        handled += 1;
        if let Err(Errno(errno)) = unlink(path) {
            let name = core::str::from_utf8(path).unwrap_or("?");
            eprintln!("rm: {name}: cannot remove (errno {errno})");
            code = 1;
        }
    }
    if handled == 0 {
        eprintln!("usage: rm <path>...");
        return 2;
    }
    code
}
