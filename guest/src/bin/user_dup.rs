//! MiniOS guestの`dup2`サンプル。
//!
//! `pipe`のwrite端を`dup2`でfd 1へ移してから`DOCS/CHILD.ELF`をspawnし、
//! childのstdoutをpipeへ流す。parentは退避しておいたconsoleでfd 1を戻し、
//! `waitpid`の後にpipeのread端から`spawn-child\n`を読んで自分のstdoutへ
//! 写す。childのstdoutはconsoleへ出ないため、frame列ではchildのExitが
//! この写しより先に来る。
//! 併せて`dup2`の`EBADF`と同一fdの規約、複製したpipe端がすべて閉じるまで
//! EOFにならないこと、fd 2の`close`、16個目までの`open`と17個目の
//! `EMFILE`も確かめ、`dup verified`を出して42で終了する。
//! 失敗時はpanicし、70で終了する。E2Eのuser-dup検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{
    EBADF, EMFILE, FD_TABLE_LEN, FIRST_FILE_FD, MAX_OPEN_FILES, STDERR, STDOUT,
};
use minios_guest::{
    Args, Errno,
    fs::File,
    io, println,
    process::{dup2, pipe, spawn, wait},
    sys::{sys_close, sys_write},
};

const SUCCESS_EXIT: i32 = 42;
/// consoleのstdoutを退避しておくfd。tableの末尾を使う。
const SAVED_STDOUT: usize = FD_TABLE_LEN - 1;
/// pipe端を複製する先のfd。
const COPY_FD: usize = 10;
/// disk image fixtureの`DOCS/CHILD.ELF`。fd 1へ`spawn-child\n`を書き、
/// pid 1 + 41 = 42で終了する。
const CHILD_PATH: &[u8] = b"DOCS/CHILD.ELF";
const CHILD_PID: usize = 1;
const CHILD_OUTPUT: &[u8] = b"spawn-child\n";
const OPEN_PATH: &[u8] = b"HELLO.TXT";

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // 範囲外や未割当のoldfdと範囲外のnewfdはEBADF、同一fdは何も変えない。
    assert_eq!(dup2(FD_TABLE_LEN, 5), Err(Errno(EBADF)));
    assert_eq!(dup2(7, 5), Err(Errno(EBADF)));
    assert_eq!(dup2(STDOUT, FD_TABLE_LEN), Err(Errno(EBADF)));
    assert_eq!(dup2(STDOUT, STDOUT), Ok(STDOUT));

    // childのstdoutをpipeへ向けてからspawnし、parentのstdoutを戻す。
    let (mut reader, writer) = pipe().unwrap();
    assert_eq!(dup2(STDOUT, SAVED_STDOUT), Ok(SAVED_STDOUT));
    let saved = File::from_raw_fd(SAVED_STDOUT);
    assert_eq!(dup2(writer.as_raw_fd(), STDOUT), Ok(STDOUT));
    assert_eq!(spawn(CHILD_PATH, &[]), Ok(CHILD_PID));
    assert_eq!(dup2(saved.as_raw_fd(), STDOUT), Ok(STDOUT));
    saved.close().unwrap();
    writer.close().unwrap();
    assert_eq!(wait(CHILD_PID), Ok(SUCCESS_EXIT));

    // childの出力はpipeにあり、write端はすべて閉じたので次はEOF。
    let mut buffer = [0u8; 32];
    let count = reader.read(&mut buffer).unwrap();
    assert_eq!(&buffer[..count], CHILD_OUTPUT);
    assert_eq!(reader.read(&mut buffer), Ok(0));
    reader.close().unwrap();
    io::write_all(STDOUT, &buffer[..count]).unwrap();

    // 複製したwrite端は元を閉じても生きており、dup2で上書きして
    // 最後のwrite端が消えるとEOFになる。
    let (mut reader, writer) = pipe().unwrap();
    assert_eq!(dup2(writer.as_raw_fd(), COPY_FD), Ok(COPY_FD));
    writer.close().unwrap();
    let mut copy = File::from_raw_fd(COPY_FD);
    assert_eq!(copy.write(b"x"), Ok(1));
    assert_eq!(reader.read(&mut buffer[..1]), Ok(1));
    assert_eq!(dup2(reader.as_raw_fd(), COPY_FD), Ok(COPY_FD));
    assert_eq!(reader.read(&mut buffer[..1]), Ok(0));
    assert_eq!(copy.write(b"x"), Err(Errno(EBADF)));
    copy.close().unwrap();
    reader.close().unwrap();

    // fd 2も普通のslotなので閉じられ、consoleをdup2で戻せる。閉じたfdへの
    // probeは生のsyscallで行う。
    assert_eq!(sys_close(STDERR), 0);
    assert_eq!(sys_write(STDERR, b"x".as_ptr(), 1), EBADF);
    assert_eq!(sys_close(STDERR), EBADF);
    assert_eq!(dup2(STDOUT, STDERR), Ok(STDERR));

    // fd 0..2を除いてMAX_OPEN_FILES個まで開け、次はEMFILE。
    for index in 0..MAX_OPEN_FILES {
        let fd = File::open(OPEN_PATH).unwrap().into_raw_fd();
        assert_eq!(fd, FIRST_FILE_FD + index);
    }
    assert_eq!(File::open(OPEN_PATH).err(), Some(Errno(EMFILE)));
    for fd in FIRST_FILE_FD..FD_TABLE_LEN {
        File::from_raw_fd(fd).close().unwrap();
    }

    println!("dup verified");
    SUCCESS_EXIT
}
