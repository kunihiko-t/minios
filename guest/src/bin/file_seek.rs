//! MiniOS guestの`lseek`/`pread`/`pwrite`サンプル。
//!
//! `pread`/`pwrite`がfd保持のoffsetを動かさないこと、`lseek`の
//! SET/CUR/ENDとbeyond-EOF、方向性fdと未割当fdへの`EBADF`、
//! consoleを指すfdへの`ESPIPE`、
//! `pwrite`の`offset>size`に対する`EINVAL`を確かめ、最後に書き換えた
//! fileを読み戻して内容をstdoutへ出してから42で終了する。
//! 失敗時はpanicし、70で終了する。E2Eのfile-seek検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EBADF, EINVAL, ESPIPE, FIRST_FILE_FD, SEEK_SET, STDIN};
use minios_guest::{
    Args, Errno,
    fs::{File, SeekFrom},
    println,
    sys::{sys_lseek, sys_pread},
};

/// 読み書き位置を確かめるfile名。
const PATH: &[u8] = b"SEEK.TXT";
const WRITE_PATH: &[u8] = b"SEEKW.TXT";
const CONTENT: &[u8] = b"0123456789";
const BUFFER_LEN: usize = 64;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let mut buffer = [0u8; BUFFER_LEN];

    // 既知の内容を持つfileを用意してread-onlyで開き直す。
    let mut file = File::create(PATH).unwrap();
    assert_eq!(file.write(CONTENT), Ok(CONTENT.len()));
    file.close().unwrap();
    let mut rfile = File::open(PATH).unwrap();

    // preadはfdのoffsetを動かさない: offset 4から読んでも
    // 続くreadは先頭から進む。
    assert_eq!(rfile.pread(&mut buffer[..4], 4), Ok(4));
    assert_eq!(&buffer[..4], b"4567");
    assert_eq!(rfile.read(&mut buffer[..2]), Ok(2));
    assert_eq!(&buffer[..2], b"01");

    // lseek SET/CUR/END。readは動いたoffsetから続く。
    assert_eq!(rfile.seek(SeekFrom::Start(4)), Ok(4));
    assert_eq!(rfile.read(&mut buffer[..2]), Ok(2));
    assert_eq!(&buffer[..2], b"45");
    assert_eq!(rfile.seek(SeekFrom::Current(-3)), Ok(3));
    assert_eq!(rfile.read(&mut buffer[..1]), Ok(1));
    assert_eq!(&buffer[..1], b"3");
    assert_eq!(rfile.seek(SeekFrom::End(-2)), Ok(8));
    assert_eq!(rfile.read(&mut buffer[..2]), Ok(2));
    assert_eq!(&buffer[..2], b"89");

    // EOF以降とEOF越えのseek: readは0を返す。
    assert_eq!(rfile.seek(SeekFrom::End(0)), Ok(10));
    assert_eq!(rfile.read(&mut buffer[..1]), Ok(0));
    assert_eq!(rfile.seek(SeekFrom::Start(64)), Ok(64));
    assert_eq!(rfile.read(&mut buffer[..1]), Ok(0));
    // 負になる結果と未知のwhence、file以外のfdはerrnoを返す。`SeekFrom`で
    // 表せない引数は生のsyscallで渡す。
    let rfd = rfile.as_raw_fd();
    assert_eq!(sys_lseek(rfd, -1, SEEK_SET), EINVAL);
    assert_eq!(rfile.seek(SeekFrom::End(-20)), Err(Errno(EINVAL)));
    assert_eq!(sys_lseek(rfd, 0, 99), EINVAL);
    // consoleはpipe端と同じく位置を持たない。
    assert_eq!(sys_lseek(STDIN, 0, SEEK_SET), ESPIPE);
    // read-only fdへのpwriteと未割当fdへのpreadはEBADF。
    assert_eq!(rfile.pwrite(b"x", 0), Err(Errno(EBADF)));
    assert_eq!(
        sys_pread(FIRST_FILE_FD + 3, buffer.as_mut_ptr(), 1, 0),
        EBADF
    );
    rfile.close().unwrap();

    // pwriteはfdのoffsetを動かさない: offset 1へ"ZZ"を書いても
    // 続くwriteはfd offset 4へ追記する。
    let mut wfile = File::create(WRITE_PATH).unwrap();
    assert_eq!(wfile.write(b"aaaa"), Ok(4));
    assert_eq!(wfile.pwrite(b"ZZ", 1), Ok(2));
    assert_eq!(wfile.write(b"b"), Ok(1));
    // writable fdへのpreadと、sizeを越えるpwriteはerrnoを返す。
    assert_eq!(wfile.pread(&mut buffer[..1], 0), Err(Errno(EBADF)));
    assert_eq!(wfile.pwrite(b"x", 10), Err(Errno(EINVAL)));
    // lseekで先頭へ戻して上書きする。fileは"QZZab"になる。
    assert_eq!(wfile.seek(SeekFrom::Start(0)), Ok(0));
    assert_eq!(wfile.write(b"Q"), Ok(1));
    wfile.close().unwrap();

    // 書き換えた内容をread-onlyで読み戻して照合する。
    let mut vfile = File::open(WRITE_PATH).unwrap();
    assert_eq!(vfile.read(&mut buffer), Ok(5));
    assert_eq!(&buffer[..5], b"QZZab");
    vfile.close().unwrap();

    // 検証済みの旨をstdoutへ流し、verifierへ届ける。
    println!("seek verified");
    42
}
