//! MiniOS guestのpipeサンプル。
//!
//! `pipe`でread/write両端のfdを作り、`spawn`したchildへ両端を継承させて
//! parent→childへbyteを流すことを確認する。childはdisk image fixtureの
//! `DOCS/PIPECH.ELF`で、継承したfd 3（read端）から読んだ11 byteを
//! stdoutへ写して42で終了する。parentは`waitpid`でblockするためchildの
//! stdoutとexit frameは必ずparentの`pipe verified`より先に出る＝確定的な
//! frame列になる。
//! 併せてpipe fd固有の規約——`EFAULT`・`ESPIPE`・方向違反の`EBADF`・
//! write端全閉後のEOF・read端全閉後の`EPIPE`とslot再利用——も検査する。
//! 失敗時はpanicし、70で終了する。E2Eのfile-pipe検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EBADF, EFAULT, EPIPE, ESPIPE, FIRST_FILE_FD, STAT_KIND_PIPE, Stat};
use minios_guest::{
    Args, Errno,
    fs::SeekFrom,
    println,
    process::{pipe, spawn, wait},
    sys::sys_pipe,
};

/// 正常終了code。childもparentも42で終わる。
const SUCCESS_EXIT: i32 = 42;
/// 自分がspawnするchildはtableへ2番目にinsertされるのでpid 1。
const CHILD_PID: usize = 1;
const CHILD_PATH: &[u8] = b"DOCS/PIPECH.ELF";
/// pipe経由でchildへ流すbyte列。PIPECH.ELFは11 byteを期待する。
const PAYLOAD: &[u8] = b"pipe-bytes\n";

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // out pointerがuser range外ならEFAULT。sourceへ触れないことを
    // fd未割当で間接確認する。
    assert_eq!(sys_pipe(core::ptr::null_mut()), EFAULT);

    let (mut reader, mut writer) = pipe().unwrap();
    assert_eq!(reader.as_raw_fd(), FIRST_FILE_FD);
    assert_eq!(writer.as_raw_fd(), FIRST_FILE_FD + 1);

    // pipe端のmetadata規約: kind=PIPE, size=0。
    assert_eq!(
        reader.stat(),
        Ok(Stat {
            size: 0,
            kind: STAT_KIND_PIPE
        })
    );

    // pipe端はseekできず、read端へのwrite・write端へのreadはEBADF。
    assert_eq!(reader.seek(SeekFrom::Start(0)), Err(Errno(ESPIPE)));
    assert_eq!(reader.write(b"x"), Err(Errno(EBADF)));
    let mut sink = [0u8; 4];
    assert_eq!(writer.read(&mut sink[..1]), Err(Errno(EBADF)));

    // childを起動する。PIPECH.ELFは継承したfd 3から11 byteを読み、
    // stdoutへ写して42で終了する。
    assert_eq!(spawn(CHILD_PATH, &[]), Ok(CHILD_PID));
    assert_eq!(writer.write(PAYLOAD), Ok(PAYLOAD.len()));

    // parentはここでblockされ、childのstdoutとexit frameが先に出てから
    // code 42を回収する。
    assert_eq!(wait(CHILD_PID), Ok(SUCCESS_EXIT));

    // write端をすべて閉じるとreadはEOF(0)を返す。childの継承したwrite端も
    // childのexitで閉じているため、parent側のcloseでwrite端は0になる。
    writer.close().unwrap();
    let mut eof = [0xAAu8; 4];
    assert_eq!(reader.read(&mut eof), Ok(0));
    reader.close().unwrap();

    // 両端の閉じたslotは再利用される。read端だけ先に閉じたpipeへの
    // writeはEPIPEを返す。
    let (reader, mut writer) = pipe().unwrap();
    assert_eq!(reader.as_raw_fd(), FIRST_FILE_FD);
    assert_eq!(writer.as_raw_fd(), FIRST_FILE_FD + 1);
    reader.close().unwrap();
    assert_eq!(writer.write(b"x"), Err(Errno(EPIPE)));
    writer.close().unwrap();

    println!("pipe verified");
    SUCCESS_EXIT
}
