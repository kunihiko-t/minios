//! user shellの`ls`（`BIN/LS.ELF`）。
//!
//! 引数のdirectoryの中身を、`readdir`の順に1行1件でstdoutへ書く。
//! directoryは名前の後ろに`/`を付ける。引数がなければroot directoryを
//! 一覧する。fileを指す引数はその名前だけを書く。開けない引数は
//! stderrへ1行出して次へ進み、最後に1で終了する。

#![no_std]
#![no_main]

use minios_abi::syscall::{DirEnt, STAT_KIND_DIR};
use minios_guest::{
    Args, Errno, eprintln,
    fs::{read_dir, stat},
    println,
};

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

/// 1件の名前を書く。directoryなら`/`を付ける。
fn print_entry(entry: &DirEnt) {
    let name = text(&entry.name[..entry.name_len as usize]);
    let suffix = if entry.kind == STAT_KIND_DIR { "/" } else { "" };
    println!("{name}{suffix}");
}

/// `path`を一覧する。失敗はerrnoを返す。
fn list(path: &[u8]) -> Result<(), Errno> {
    if !path.is_empty() && stat(path)?.kind != STAT_KIND_DIR {
        println!("{}", text(path));
        return Ok(());
    }
    for entry in read_dir(path) {
        print_entry(&entry?);
    }
    Ok(())
}

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut code = 0;
    let mut listed = 0;
    for path in args.skip(1) {
        listed += 1;
        if let Err(Errno(errno)) = list(path) {
            eprintln!("ls: {}: cannot list (errno {errno})", text(path));
            code = 1;
        }
    }
    if listed == 0 && list(b"").is_err() {
        eprintln!("ls: cannot list the root directory");
        code = 1;
    }
    code
}
