//! MiniOS guestの`mkdir`/`rmdir`サンプル。
//!
//! directoryの作成、duplicate作成の`EEXIST`、非空directoryの`ENOTEMPTY`、
//! fileの`rmdir`拒否、nested directory、fileを消した後のrmdir成功、
//! 削除済みdirectory内のfileが`ENOENT`になることを確認し、42で終了する。
//! 失敗時はpanicし、70で終了する。E2Eのfile-mkdir検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EEXIST, EINVAL, ENOENT, ENOTDIR, ENOTEMPTY};
use minios_guest::{
    Args, Errno,
    fs::{File, mkdir, rmdir, unlink},
    println,
};

/// 作るdirectoryと、その中に置くfile。
const DIR_PATH: &[u8] = b"NEWDIR";
const INNER_PATH: &[u8] = b"NEWDIR/F.TXT";
const NESTED_PATH: &[u8] = b"DOCS/NEST";
const FILE_PATH: &[u8] = b"HELLO.TXT";
const MISSING_PATH: &[u8] = b"MISSING";
const UNDER_MISSING_PATH: &[u8] = b"MISSING/X";
const LFN_PATH: &[u8] = b"long name";
const PAYLOAD: &[u8] = b"inside dir\n";
const BUFFER_LEN: usize = 64;

/// `DIR_PATH`を作り、中の`INNER_PATH`へ`PAYLOAD`を書く。
fn make_dir_with_file() {
    mkdir(DIR_PATH).unwrap();
    let mut file = File::create(INNER_PATH).unwrap();
    assert_eq!(file.write(PAYLOAD), Ok(PAYLOAD.len()));
    file.close().unwrap();
}

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    make_dir_with_file();

    // errno契約：duplicate mkdir、非空directoryのrmdir、fileのrmdir、
    // 不在path、非8.3名。
    assert_eq!(mkdir(DIR_PATH), Err(Errno(EEXIST)));
    assert_eq!(rmdir(DIR_PATH), Err(Errno(ENOTEMPTY)));
    assert_eq!(rmdir(FILE_PATH), Err(Errno(ENOTDIR)));
    assert_eq!(rmdir(MISSING_PATH), Err(Errno(ENOENT)));
    assert_eq!(mkdir(LFN_PATH), Err(Errno(EINVAL)));
    assert_eq!(mkdir(UNDER_MISSING_PATH), Err(Errno(ENOENT)));

    // nested作成：既存subdirectoryの下へ作れる（`..`が非root親を指す）。
    mkdir(NESTED_PATH).unwrap();
    rmdir(NESTED_PATH).unwrap();

    // fileを消せばdirectoryを外せる。中のfileもdirectoryも見えなくなる。
    unlink(INNER_PATH).unwrap();
    rmdir(DIR_PATH).unwrap();
    assert_eq!(File::open(INNER_PATH).err(), Some(Errno(ENOENT)));
    assert_eq!(rmdir(DIR_PATH), Err(Errno(ENOENT)));

    // 削除跡slotへ同じ名前で再作成し、中のfileを読み戻して内容を照合する。
    make_dir_with_file();
    let mut file = File::open(INNER_PATH).unwrap();
    let mut buffer = [0u8; BUFFER_LEN];
    assert_eq!(file.read(&mut buffer[..PAYLOAD.len()]), Ok(PAYLOAD.len()));
    assert_eq!(&buffer[..PAYLOAD.len()], PAYLOAD);
    file.close().unwrap();
    unlink(INNER_PATH).unwrap();
    rmdir(DIR_PATH).unwrap();

    println!("mkdir verified");
    42
}
