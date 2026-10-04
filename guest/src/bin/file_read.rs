//! MiniOS guestの`read_file`サンプル。
//!
//! `DOCS/NOTE.TXT`を`read_file`で読み、内容をstdoutへ書いて42で終了する。
//! syscall失敗時はpanicし、70で終了する。E2Eのfile検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::STDOUT;
use minios_guest::{Args, io, sys::sys_read_file};

/// disk image fixtureの`DOCS/NOTE.TXT`。
const PATH: &[u8] = b"DOCS/NOTE.TXT";
/// guest stack上のfile buffer。fixture fileは17 byteで収まる。
const BUFFER_LEN: usize = 512;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let mut buffer = [0u8; BUFFER_LEN];
    // `read_file`は検証専用のsyscallで、libraryの型を持たない。
    let read = sys_read_file(PATH.as_ptr(), PATH.len(), buffer.as_mut_ptr(), BUFFER_LEN);
    assert!(read >= 0);
    io::write_all(STDOUT, &buffer[..read as usize]).unwrap();
    42
}
