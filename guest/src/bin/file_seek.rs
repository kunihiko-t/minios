//! MiniOS guestの`lseek`/`pread`/`pwrite`サンプル。
//!
//! `pread`/`pwrite`がfd保持のoffsetを動かさないこと、`lseek`の
//! SET/CUR/ENDとbeyond-EOF、方向性fdと未割当fdへの`EBADF`、
//! `pwrite`の`offset>size`に対する`EINVAL`を確かめ、最後に書き換えた
//! fileを読み戻して内容をstdoutへ出してから42で終了する。
//! 失敗時は70で終了する。E2Eのfile-seek検査が使う。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{
    EBADF, EINVAL, FIRST_FILE_FD, SEEK_CUR, SEEK_END, SEEK_SET, STDIN, STDOUT, SyscallNumber,
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

macro_rules! sys1 {
    ($number:expr, $a0:expr) => {{
        let returned: isize;
        // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
        unsafe {
            asm!(
                "ecall",
                inlateout("a0") $a0 as isize => returned,
                in("a7") $number as usize,
                options(nostack),
            );
        }
        returned
    }};
}

macro_rules! sys3 {
    ($number:expr, $a0:expr, $a1:expr, $a2:expr) => {{
        let returned: isize;
        // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
        unsafe {
            asm!(
                "ecall",
                inlateout("a0") $a0 as isize => returned,
                in("a1") $a1 as isize,
                in("a2") $a2 as isize,
                in("a7") $number as usize,
                options(nostack),
            );
        }
        returned
    }};
}

/// MiniOS ABIの`create`を呼ぶ。戻り値はwritable fdか負のerrno。
fn sys_create(path: &[u8]) -> isize {
    sys3!(SyscallNumber::Create, path.as_ptr() as usize, path.len(), 0)
}

/// MiniOS ABIの`open`を呼ぶ。戻り値はread-only fdか負のerrno。
fn sys_open(path: &[u8]) -> isize {
    sys3!(SyscallNumber::Open, path.as_ptr() as usize, path.len(), 0)
}

/// MiniOS ABIの`read`を呼ぶ。戻り値は読んだbyte数、EOFは0、負はerrno。
fn sys_read(fd: usize, buffer: &mut [u8], len: usize) -> isize {
    sys3!(SyscallNumber::Read, fd, buffer.as_mut_ptr() as usize, len)
}

/// MiniOS ABIの`pread`を呼ぶ。fdのoffsetを動かさず`offset`から読む。
fn sys_pread(fd: usize, buffer: &mut [u8], len: usize, offset: u64) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // bufferはU+W検証済みのstack bufferである。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd as isize => returned,
            in("a1") buffer.as_mut_ptr() as usize,
            in("a2") len,
            in("a3") offset,
            in("a7") SyscallNumber::Pread as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`write`を呼ぶ。戻り値は書いたbyte数か負のerrno。
fn sys_write(fd: usize, data: &[u8]) -> isize {
    sys3!(SyscallNumber::Write, fd, data.as_ptr() as usize, data.len())
}

/// MiniOS ABIの`pwrite`を呼ぶ。fdのoffsetを動かさず`offset`へ書く。
fn sys_pwrite(fd: usize, data: &[u8], offset: u64) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // dataはU+R検証済みのstatic rangeである。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd as isize => returned,
            in("a1") data.as_ptr() as usize,
            in("a2") data.len(),
            in("a3") offset,
            in("a7") SyscallNumber::Pwrite as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`lseek`を呼ぶ。戻り値は新しいoffsetか負のerrno。
fn sys_lseek(fd: usize, offset: isize, whence: usize) -> isize {
    sys3!(SyscallNumber::Lseek, fd, offset, whence)
}

/// MiniOS ABIの`close`を呼ぶ。戻り値は0か負のerrno。
fn sys_close(fd: usize) -> isize {
    sys1!(SyscallNumber::Close, fd)
}

/// MiniOS ABIの`exit`。kernelはこの呼び出しの後guestへ戻らない。
fn sys_exit(code: u32) -> ! {
    // Safety: exitのecallはresumeしない契約のため、noreturnでよい。
    unsafe {
        asm!(
            "ecall",
            in("a0") code as isize,
            in("a7") SyscallNumber::Exit as usize,
            options(noreturn),
        );
    }
}

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
    let fd = sys_create(PATH);
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, CONTENT) != CONTENT.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let rfd = sys_open(PATH);
    if rfd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let rfd = rfd as usize;

    // preadはfdのoffsetを動かさない: offset 4から読んでも
    // 続くreadは先頭から進む。
    if sys_pread(rfd, &mut buffer, 4, 4) != 4 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 4, b"4567");
    if sys_read(rfd, &mut buffer, 2) != 2 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 2, b"01");

    // lseek SET/CUR/END。readは動いたoffsetから続く。
    if sys_lseek(rfd, 4, SEEK_SET) != 4 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, &mut buffer, 2) != 2 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 2, b"45");
    if sys_lseek(rfd, -3, SEEK_CUR) != 3 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, &mut buffer, 1) != 1 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 1, b"3");
    if sys_lseek(rfd, -2, SEEK_END) != 8 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, &mut buffer, 2) != 2 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 2, b"89");

    // EOF以降とEOF越えのseek: readは0を返す。
    if sys_lseek(rfd, 0, SEEK_END) != 10 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, &mut buffer, 1) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_lseek(rfd, 64, SEEK_SET) != 64 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(rfd, &mut buffer, 1) != 0 {
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
    if sys_pwrite(rfd, b"x", 0) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_pread(FIRST_FILE_FD + 3, &mut buffer, 1, 0) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(rfd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // pwriteはfdのoffsetを動かさない: offset 1へ"ZZ"を書いても
    // 続くwriteはfd offset 4へ追記する。
    let wfd = sys_create(WRITE_PATH);
    if wfd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let wfd = wfd as usize;
    if sys_write(wfd, b"aaaa") != 4 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_pwrite(wfd, b"ZZ", 1) != 2 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(wfd, b"b") != 1 {
        sys_exit(FAILURE_EXIT);
    }
    // writable fdへのpreadと、sizeを越えるpwriteはerrnoを返す。
    if sys_pread(wfd, &mut buffer, 1, 0) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_pwrite(wfd, b"x", 10) != EINVAL {
        sys_exit(FAILURE_EXIT);
    }
    // lseekで先頭へ戻して上書きする。fileは"QZZab"になる。
    if sys_lseek(wfd, 0, SEEK_SET) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(wfd, b"Q") != 1 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(wfd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 書き換えた内容をread-onlyで読み戻して照合する。
    let vfd = sys_open(WRITE_PATH);
    if vfd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(vfd as usize, &mut buffer, BUFFER_LEN) != 5 {
        sys_exit(FAILURE_EXIT);
    }
    expect_bytes(&buffer, 5, b"QZZab");
    if sys_close(vfd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 検証済みの旨をstdoutへ流し、verifierへ届ける。
    if sys_write(STDOUT, PAYLOAD) < 0 {
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
