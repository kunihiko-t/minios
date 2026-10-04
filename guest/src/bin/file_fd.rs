//! MiniOS guestの`open`/`read(fd)`/`close`サンプル。
//!
//! `DOCS/NOTE.TXT`を開き、分割readでoffsetが進むこと、EOF、close後の
//! `EBADF`、存在しないfileの`ENOENT`を確認してから内容をstdoutへ書き、
//! 42で終了する。失敗時はpanicし、70で終了する。E2Eのfile-fd検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EBADF, ENOENT, FIRST_FILE_FD, STDOUT};
use minios_guest::{
    Args, Errno,
    fs::File,
    io,
    sys::{sys_close, sys_read},
};

/// disk image fixtureの`DOCS/NOTE.TXT`（"note inside docs\n"、17 byte）。
const PATH: &[u8] = b"DOCS/NOTE.TXT";
const MISSING_PATH: &[u8] = b"MISSING.TXT";
/// file内容と同じ長さの蓄積buffer。
const BUFFER_LEN: usize = 64;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let mut file = File::open(PATH).unwrap();
    let fd = file.as_raw_fd();
    assert!(fd >= FIRST_FILE_FD);

    // 分割readでoffsetが進み、最後はEOFの0を返すことを確認する。
    let mut buffer = [0u8; BUFFER_LEN];
    let mut total = 0usize;
    loop {
        let read = file.read(&mut buffer[total..total + 8]).unwrap();
        if read == 0 {
            break;
        }
        total += read;
    }
    assert_eq!(total, b"note inside docs\n".len());
    io::write_all(STDOUT, &buffer[..total]).unwrap();

    // close済みfdと存在しないfileのerrnoを確認する。閉じたfdへのprobeは
    // 生のsyscallで行う。
    file.close().unwrap();
    assert_eq!(sys_close(fd), EBADF);
    assert_eq!(sys_read(fd, buffer.as_mut_ptr(), 8), EBADF);
    assert_eq!(File::open(MISSING_PATH).err(), Some(Errno(ENOENT)));
    42
}
