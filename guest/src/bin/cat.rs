//! user shellの`cat`（`BIN/CAT.ELF`）。
//!
//! file引数があれば順に開いてstdoutへ写し、なければstdinをEOFまで写す。
//! 開けないfileはstderrへ1行出して次へ進み、最後に1で終了する。

#![no_std]
#![no_main]

use minios_abi::syscall::{STDIN, STDOUT};
use minios_guest::{Args, Errno, eprintln, fs::File, io};

const CHUNK_LEN: usize = 512;

/// `fd`をEOFまで読み、stdoutへ書く。
fn copy(fd: usize) -> Result<(), Errno> {
    let mut chunk = [0u8; CHUNK_LEN];
    loop {
        let read = io::read(fd, &mut chunk)?;
        if read == 0 {
            return Ok(());
        }
        io::write_all(STDOUT, &chunk[..read])?;
    }
}

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut code = 0;
    let mut files = 0;
    for path in args.skip(1) {
        files += 1;
        let name = core::str::from_utf8(path).unwrap_or("?");
        match File::open(path) {
            Ok(file) => copy(file.as_raw_fd()).unwrap(),
            Err(Errno(errno)) => {
                eprintln!("cat: {name}: cannot open (errno {errno})");
                code = 1;
            }
        }
    }
    if files == 0 {
        copy(STDIN).unwrap();
    }
    code
}
