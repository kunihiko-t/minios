//! MiniOS guestのfd継承サンプル。
//!
//! `open`したfileのfdを`lseek`でoffset 4へ進めてから`spawn`し、childが
//! 継承したfd 3からoffset以降の13 byte（`" inside docs\n"`）をstdoutへ
//! 写すことを確認する。child側のreadがparentのoffsetを動かさないこと
//! （snapshot semantics）もparent側の再読で確かめて42で終了する。
//! childはdisk image fixtureの`DOCS/FDCHILD.ELF`で、fd 3をreadして
//! 内容をstdoutへ出す。parentが`waitpid`でblockするため、childの
//! stdoutとexit frameは必ずparentの`fd-inherit verified`より先に出る
//! ＝確定的なframe列になる。
//! 失敗時はpanicし、70で終了する。E2Eのfile-fdinherit検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::FIRST_FILE_FD;
use minios_guest::{
    Args,
    fs::{File, SeekFrom},
    println,
    process::{spawn, wait},
};

/// 正常終了code。childもparentも42で終わる。
const SUCCESS_EXIT: i32 = 42;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
/// `DOCS/NOTE.TXT`の内容（17 byte）。offset 4以降が継承fd経由で読める。
const NOTE_PATH: &[u8] = b"DOCS/NOTE.TXT";
/// `DOCS/NOTE.TXT`のbyte長（`"note inside docs\n"`）。
const NOTE_LEN: usize = 17;
const CHILD_PATH: &[u8] = b"DOCS/FDCHILD.ELF";
/// offset 4以降の13 byte。childがstdoutへ写し、parent側でも再読して照合する。
const EXPECTED: &[u8] = b" inside docs\n";

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // NOTE.TXTを開いてoffset 4へ進める。childはこの位置をsnapshotで引き継ぐ。
    let mut note = File::open(NOTE_PATH).unwrap();
    assert_eq!(note.as_raw_fd(), FIRST_FILE_FD);
    let offset = (NOTE_LEN - EXPECTED.len()) as u64;
    assert_eq!(note.seek(SeekFrom::Start(offset)), Ok(4));

    // childを起動する。FDCHILD.ELFはfd 3をreadしてstdoutへ写し42で終了する。
    assert_eq!(spawn(CHILD_PATH, &[]), Ok(CHILD_PID));

    // childの完了を待つ。parentはここでblockされ、childのstdoutとexit
    // frameが先に出てからcode 42を回収する。
    assert_eq!(wait(CHILD_PID), Ok(SUCCESS_EXIT));

    // child側のreadがparentのoffsetを動かさないこと（snapshot semantics）
    // をparent側の再読で確かめる。
    let mut buffer = [0u8; 64];
    assert_eq!(note.read(&mut buffer[..EXPECTED.len()]), Ok(EXPECTED.len()));
    assert_eq!(&buffer[..EXPECTED.len()], EXPECTED);

    println!("fd-inherit verified");
    SUCCESS_EXIT
}
