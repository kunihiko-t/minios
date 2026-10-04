//! user shellの`wc`（`BIN/WC.ELF`）。
//!
//! stdin（file引数があればその全fileの合計）の行数、単語数、byte数を
//! `<lines> <words> <bytes>\n`の1行でstdoutへ書き、0で終了する。
//! 単語は空白、tab、改行で区切る。開けないfileは1で終了する。

#![no_std]
#![no_main]

use minios_abi::syscall::STDIN;
use minios_guest::{Args, Errno, eprintln, fs::File, io, println};

const CHUNK_LEN: usize = 512;

#[derive(Default)]
struct Counts {
    lines: usize,
    words: usize,
    bytes: usize,
    in_word: bool,
}

impl Counts {
    /// `fd`をEOFまで読んで数える。
    fn add(&mut self, fd: usize) -> Result<(), Errno> {
        let mut chunk = [0u8; CHUNK_LEN];
        loop {
            let read = io::read(fd, &mut chunk)?;
            if read == 0 {
                return Ok(());
            }
            self.bytes += read;
            for &byte in &chunk[..read] {
                let space = matches!(byte, b' ' | b'\t' | b'\n' | b'\r');
                if byte == b'\n' {
                    self.lines += 1;
                }
                if !space && !self.in_word {
                    self.words += 1;
                }
                self.in_word = !space;
            }
        }
    }
}

minios_guest::entry!(main);

fn main(args: Args) -> i32 {
    let mut counts = Counts::default();
    let mut files = 0;
    for path in args.skip(1) {
        files += 1;
        match File::open(path) {
            Ok(file) => counts.add(file.as_raw_fd()).unwrap(),
            Err(Errno(errno)) => {
                let name = core::str::from_utf8(path).unwrap_or("?");
                eprintln!("wc: {name}: cannot open (errno {errno})");
                return 1;
            }
        }
    }
    if files == 0 {
        counts.add(STDIN).unwrap();
    }
    println!("{} {} {}", counts.lines, counts.words, counts.bytes);
    0
}
