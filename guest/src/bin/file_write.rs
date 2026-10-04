//! MiniOS guestの`create`/`write(fd)`サンプル。
//!
//! `GUEST.TXT`を作成して内容を書き込み、閉じてから読み専用で開き直して
//! 内容を検証する。writable fdへの`read`とread-only fdへの`write`が
//! `EBADF`を返すこと、directoryへの`create`が`EISDIR`を返すことも
//! 確認してから42で終了する。失敗時はpanicし、70で終了する。
//! E2Eのfile-write検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EBADF, EISDIR, STDOUT};
use minios_guest::{Args, Errno, fs::File, io};

/// 作成するfile名と内容。内容はframe verifierが照合する。
const PATH: &[u8] = b"GUEST.TXT";
const DIR_PATH: &[u8] = b"DOCS";
const PAYLOAD: &[u8] = b"written by guest\n";
const BUFFER_LEN: usize = 64;

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    // 作成して書き込む。writable fdへのreadはEBADFでなければならない。
    let mut file = File::create(PATH).unwrap();
    let mut scratch = [0u8; BUFFER_LEN];
    assert_eq!(file.read(&mut scratch[..8]), Err(Errno(EBADF)));
    assert_eq!(file.write(PAYLOAD), Ok(PAYLOAD.len()));
    file.close().unwrap();

    // 読み専用で開き直し、書いた内容がそのまま読めることを確認する。
    let mut file = File::open(PATH).unwrap();
    let mut buffer = [0u8; BUFFER_LEN];
    assert_eq!(file.read(&mut buffer), Ok(PAYLOAD.len()));
    assert_eq!(&buffer[..PAYLOAD.len()], PAYLOAD);
    // read-only fdへのwriteはEBADFでなければならない。
    assert_eq!(file.write(PAYLOAD), Err(Errno(EBADF)));
    file.close().unwrap();

    // directoryのcreateはEISDIRで拒否される。
    assert_eq!(File::create(DIR_PATH).err(), Some(Errno(EISDIR)));

    // 読んだ内容をstdoutへ流し、verifierへ届ける。
    io::write_all(STDOUT, PAYLOAD).unwrap();
    42
}
