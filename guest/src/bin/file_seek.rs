//! MiniOS guestの`lseek`/`pread`/`pwrite`サンプル。
//!
//! `pread`/`pwrite`がfd保持のoffsetを動かさないこと、`lseek`の
//! SET/CUR/ENDとbeyond-EOF、方向性fdと未割当fdへの`EBADF`、
//! `pwrite`の`offset>size`に対する`EINVAL`を確かめ、最後に書き換えた
//! fileを読み戻して内容をstdoutへ出してから42で終了する。
//! 失敗時は70で終了する。E2Eのfile-seek検査が使う。

#![no_std]
#![no_main]

use core::arch::naked_asm;
use minios_abi::syscall::{
    EBADF, EINVAL, FIRST_FILE_FD, SEEK_CUR, SEEK_END, SEEK_SET, STDIN, STDOUT,
};
use minios_guest::sys::{
    sys_close, sys_create, sys_exit, sys_lseek, sys_open, sys_pread, sys_pwrite, sys_read,
    sys_write,
};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
/// 読み書き位置を確かめるfile名と、frame verifierが照合する出力。
const PATH: &[u8] = b"SEEK.TXT";
const WRITE_PATH: &[u8] = b"SEEKW.TXT";
const CONTENT: &[u8] = b"0123456789";
const PAYLOAD: &[u8] = b"seek verified\n";
const BUFFER_LEN: usize = 64;

/// bufferの先頭`len` byteが`expected`と一致しなければ70で終了する。
fn expect_bytes(buffer: &[u8], len: usize, expected: &[u8]) {
    if &buffer[..len] != expected {
        sys_exit(FAILURE_EXIT);
    }
}

/// `_start`から呼ばれるRust本体。位置指定I/Oの契約を順に確かめる。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    let mut buffer = [0u8; BUFFER_LEN];

    // 既知の内容を持つfileを用意してread-onlyで開き直す。
    let fd = sys_create(PATH.as_ptr(), PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, CONTENT.as_ptr(), CONTENT.len()) != CONTENT.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let rfd = sys_open(PATH.as_ptr(), PATH.len());
    if rfd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let rfd = rfd as usize;

    // preadはfdのoffsetを動かさない: offset 4から読んでも
    // 続くreadは先頭から進む。
    if sys_pread(rfd, buffer.as_mut_ptr(), 4, 4) != 4 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 4, b"4567");
    if sys_read(rfd, buffer.as_mut_ptr(), 2) != 2 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 2, b"01");

    // lseek SET/CUR/END。readは動いたoffsetから続く。
    if sys_lseek(rfd, 4, SEEK_SET) != 4 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, buffer.as_mut_ptr(), 2) != 2 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 2, b"45");
    if sys_lseek(rfd, -3, SEEK_CUR) != 3 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, buffer.as_mut_ptr(), 1) != 1 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 1, b"3");
    if sys_lseek(rfd, -2, SEEK_END) != 8 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, buffer.as_mut_ptr(), 2) != 2 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 2, b"89");

    // EOF以降とEOF越えのseek: readは0を返す。
    if sys_lseek(rfd, 0, SEEK_END) != 10 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, buffer.as_mut_ptr(), 1) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_lseek(rfd, 64, SEEK_SET) != 64 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, buffer.as_mut_ptr(), 1) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    // 負になる結果と未知のwhence、file以外のfdはerrnoを返す。
    if sys_lseek(rfd, -1, SEEK_SET) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }
    if sys_lseek(rfd, -20, SEEK_END) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }
    if sys_lseek(rfd, 0, 99) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }
    if sys_lseek(STDIN, 0, SEEK_SET) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    // read-only fdへのpwriteと未割当fdへのpreadはEBADF。
    if sys_pwrite(rfd, b"x".as_ptr(), b"x".len(), 0) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_pread(FIRST_FILE_FD + 3, buffer.as_mut_ptr(), 1, 0) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(rfd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // pwriteはfdのoffsetを動かさない: offset 1へ"ZZ"を書いても
    // 続くwriteはfd offset 4へ追記する。
    let wfd = sys_create(WRITE_PATH.as_ptr(), WRITE_PATH.len());
    if wfd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let wfd = wfd as usize;
    if sys_write(wfd, b"aaaa".as_ptr(), b"aaaa".len()) != 4 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_pwrite(wfd, b"ZZ".as_ptr(), b"ZZ".len(), 1) != 2 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(wfd, b"b".as_ptr(), b"b".len()) != 1 {
        sys_exit(FAILURE_EXIT);
    }
    // writable fdへのpreadと、sizeを越えるpwriteはerrnoを返す。
    if sys_pread(wfd, buffer.as_mut_ptr(), 1, 0) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_pwrite(wfd, b"x".as_ptr(), b"x".len(), 10) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }
    // lseekで先頭へ戻して上書きする。fileは"QZZab"になる。
    if sys_lseek(wfd, 0, SEEK_SET) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(wfd, b"Q".as_ptr(), b"Q".len()) != 1 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(wfd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 書き換えた内容をread-onlyで読み戻して照合する。
    let vfd = sys_open(WRITE_PATH.as_ptr(), WRITE_PATH.len());
    if vfd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(vfd as usize, buffer.as_mut_ptr(), BUFFER_LEN) != 5 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 5, b"QZZab");
    if sys_close(vfd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 検証済みの旨をstdoutへ流し、verifierへ届ける。
    if sys_write(STDOUT, PAYLOAD.as_ptr(), PAYLOAD.len()) < 0 {
        sys_exit(FAILURE_EXIT);
    }
    sys_exit(SUCCESS_EXIT);
}

/// 初期`sp`はkernelが16 byte整列済み。`a0/a1`は第一・第二引数として
/// そのまま`guest_main`へ流れるため、register操作は不要である。
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    naked_asm!(
        "call {entry}",
        "j .",
        entry = sym guest_main,
    )
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    sys_exit(FAILURE_EXIT);
}
