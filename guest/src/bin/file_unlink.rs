//! MiniOS guestの`unlink`サンプル。
//!
//! fileを作成して書き込み、`unlink`で削除する。削除したentryを指すfdが
//! `EBADF`で失効すること、削除済みのfileが`ENOENT`、directoryへの
//! `unlink`が`EISDIR`を返すことを確認し、同名で作り直して内容を読み
//! 戻してから42で終了する。失敗時はpanicし、70で終了する。
//! E2Eのfile-unlink検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EBADF, EISDIR, ENOENT, STDOUT};
use minios_guest::{
    Args, Errno,
    fs::{self, File},
    io,
    sys::{sys_close, sys_read},
};

/// 作成・削除・再作成するfile名と内容。内容はframe verifierが照合する。
const PATH: &[u8] = b"UNLINK.TXT";
const DIR_PATH: &[u8] = b"DOCS";
const MISSING_PATH: &[u8] = b"MISSING.TXT";
const PAYLOAD: &[u8] = b"file removed\n";
const BUFFER_LEN: usize = 64;

/// `PATH`を作り直して`PAYLOAD`を書き、閉じる。
fn create_with_payload() {
    let mut file = File::create(PATH).unwrap();
    assert_eq!(file.write(PAYLOAD), Ok(PAYLOAD.len()));
    file.close().unwrap();
}

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    create_with_payload();

    // 読み専用で開いたままunlinkする。開いているfdは即座に失効するため、
    // 失効の確認は生のfdで行い、dropで同じ番号を閉じないようにする。
    let fd = File::open(PATH).unwrap().into_raw_fd();
    fs::unlink(PATH).unwrap();
    let mut buffer = [0u8; BUFFER_LEN];
    assert_eq!(sys_read(fd, buffer.as_mut_ptr(), 8), EBADF);
    assert_eq!(sys_close(fd), EBADF);

    // 削除済みのfileはENOENT、directoryと不在pathへのunlinkはerrnoを返す。
    assert_eq!(File::open(PATH).err(), Some(Errno(ENOENT)));
    assert_eq!(fs::unlink(PATH), Err(Errno(ENOENT)));
    assert_eq!(fs::unlink(DIR_PATH), Err(Errno(EISDIR)));
    assert_eq!(fs::unlink(MISSING_PATH), Err(Errno(ENOENT)));

    // 削除跡slotを再利用して同名で作り直し、新しい内容が読めることを
    // 確認する。
    create_with_payload();
    let mut file = File::open(PATH).unwrap();
    assert_eq!(file.read(&mut buffer), Ok(PAYLOAD.len()));
    assert_eq!(&buffer[..PAYLOAD.len()], PAYLOAD);
    file.close().unwrap();

    // 読み戻した内容をstdoutへ流し、verifierへ届ける。
    io::write_all(STDOUT, PAYLOAD).unwrap();
    42
}
