//! user shellの`mkdir`（`BIN/MKDIR.ELF`）。
//!
//! 引数のpathを順に処理し、directoryを作る。失敗した引数はstderrへ1行出して
//! 次へ進み、最後に1で終了する。引数がなければ使い方を出して2で終了する。

#![no_std]
#![no_main]

use minios_guest::{Args, Errno, eprintln, fs::mkdir};

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut code = 0;
    let mut handled = 0;
    for path in args.skip(1) {
        handled += 1;
        if let Err(Errno(errno)) = mkdir(path) {
            let name = core::str::from_utf8(path).unwrap_or("?");
            eprintln!("mkdir: {name}: cannot create (errno {errno})");
            code = 1;
        }
    }
    if handled == 0 {
        eprintln!("usage: mkdir <path>...");
        return 2;
    }
    code
}
