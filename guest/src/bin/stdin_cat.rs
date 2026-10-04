//! MiniOS guestのstdin転送サンプル。
//!
//! stdinをEOFまで読んでstdoutへそのまま書き、42で終了する。
//! syscall失敗時はpanicし、70で終了する。E2Eのpayload-stdin検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{STDIN, STDOUT};
use minios_guest::{Args, io};

/// guest stack上のread/write buffer。要求512 byteでframe境界をまたぐ。
const CHUNK_LEN: usize = 512;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let mut chunk = [0u8; CHUNK_LEN];
    loop {
        let read = io::read(STDIN, &mut chunk).unwrap();
        if read == 0 {
            return 42;
        }
        io::write_all(STDOUT, &chunk[..read]).unwrap();
    }
}
