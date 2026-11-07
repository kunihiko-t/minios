//! user shellの`ls`（`BIN/LS.ELF`）。
//!
//! 引数のdirectoryの中身を、`readdir`の順に1行1件でstdoutへ書く。
//! directoryは名前の後ろに`/`を付ける。引数がなければ作業directoryを
//! 一覧する。fileを指す引数はその名前だけを書く。開けない引数は
//! stderrへ1行出して次へ進み、最後に1で終了する。
//!
//! 先頭の`-l`は各行を`<種類> <size> <更新日時> <名前>`の長い形にする。
//! 種類はfileが`-`、directoryが`d`で、更新日時はUTCの`YYYY-MM-DD HH:MM`、
//! 日時を持たないentryは`-`である。

#![no_std]
#![no_main]

use minios_abi::syscall::{DirEnt, STAT_KIND_DIR};
use minios_guest::{
    Args, Errno, eprintln,
    fs::{read_dir, stat},
    println,
    time::DateTime,
};

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

/// 1行に書く1件。`readdir`のentryと、fileを指す引数への`stat`の両方から作る。
struct Line<'a> {
    name: &'a str,
    directory: bool,
    size: u32,
    mtime: u64,
}

impl Line<'_> {
    /// `long`なら長い形、そうでなければ名前だけを書く。directoryの名前には
    /// `/`を付ける。
    fn print(&self, long: bool) {
        let suffix = if self.directory { "/" } else { "" };
        if !long {
            println!("{}{suffix}", self.name);
            return;
        }
        let kind = if self.directory { 'd' } else { '-' };
        if self.mtime == 0 {
            println!("{kind} {:>8} {:<16} {}{suffix}", self.size, "-", self.name);
            return;
        }
        let at = DateTime::from_unix_seconds(self.mtime);
        println!(
            "{kind} {:>8} {:04}-{:02}-{:02} {:02}:{:02} {}{suffix}",
            self.size, at.year, at.month, at.day, at.hour, at.minute, self.name
        );
    }
}

fn print_entry(entry: &DirEnt, long: bool) {
    Line {
        name: text(&entry.name[..entry.name_len as usize]),
        directory: entry.kind == STAT_KIND_DIR,
        size: entry.size,
        mtime: entry.mtime,
    }
    .print(long);
}

/// `path`を一覧する。失敗はerrnoを返す。
fn list(path: &[u8], long: bool) -> Result<(), Errno> {
    if !path.is_empty() {
        let info = stat(path)?;
        if info.kind != STAT_KIND_DIR {
            Line {
                name: text(path),
                directory: false,
                size: info.size,
                mtime: info.mtime,
            }
            .print(long);
            return Ok(());
        }
    }
    for entry in read_dir(path) {
        print_entry(&entry?, long);
    }
    Ok(())
}

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut args = args.skip(1).peekable();
    let long = args.next_if(|arg| *arg == b"-l").is_some();
    let mut code = 0;
    let mut listed = 0;
    for path in args {
        listed += 1;
        if let Err(Errno(errno)) = list(path, long) {
            eprintln!("ls: {}: cannot list (errno {errno})", text(path));
            code = 1;
        }
    }
    if listed == 0 && list(b"", long).is_err() {
        eprintln!("ls: cannot list the current directory");
        code = 1;
    }
    code
}
