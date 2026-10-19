//! pipe待ちだけで全processが止まる状況を再現する検査guest。
//!
//! read端を自分で保持したまま256 byteのpipeを満杯にし、さらに1 byte書くと
//! `EPIPE`ではなく`BlockedOnPipe`へ入る。他にrunnableなprocessはなく、
//! stdin・device I/O・timerの起床源もないため、kernelはdeadlockを診断して
//! payloadを異常終了する。`cargo xtask test file-pipe`の後半が使う。

#![no_std]
#![no_main]

use minios_guest::{Args, process::pipe};

/// pipeのbuffer容量。満杯まで書いた後のwriteがblockすることを確かめる。
const PIPE_CAPACITY: usize = 256;
/// 診断が出なければwriteから戻ってしまうため、その場合の終了code。
const SUCCESS_EXIT: i32 = 42;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let (reader, mut writer) = pipe().unwrap();
    let full = [0u8; PIPE_CAPACITY];
    assert_eq!(writer.write(&full), Ok(PIPE_CAPACITY));

    // read端がliveなのでEPIPEにはならず、満杯のpipeへのwriteは
    // BlockedOnPipeへ入る。ここから戻ればdeadlock検出が働いていない。
    let _ = writer.write(b"x");

    // deadlock検出が働かなかった場合にだけ到達する。
    reader.close().unwrap();
    SUCCESS_EXIT
}
