//! MiniOS guestの`rename`サンプル。
//!
//! fileをrenameして開いているfdが有効なまま動くこと、旧名が`ENOENT`に
//! なること、置き換えられたfileを指すfdが`EBADF`で失効すること、
//! directoryのrenameとdir/file組合せのerrno、別directoryへのmove
//! （fd追従・`..`更新・cycle拒否・cross-dir置換）を確認し、42で終了する。
//! 失敗時はpanicし、70で終了する。E2Eのfile-rename検査が使う。

#![no_std]
#![no_main]

use minios_abi::syscall::{EBADF, EINVAL, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY};
use minios_guest::{
    Args, Errno,
    fs::{File, mkdir, rename, rmdir, unlink},
    println,
    sys::sys_read,
};

/// renameするfile名と内容。内容はguestが照合する。
const OLD_PATH: &[u8] = b"RENAME.TXT";
const NEW_PATH: &[u8] = b"RENAMED.TXT";
const VICTIM_PATH: &[u8] = b"VICTIM.TXT";
const DIR_PATH: &[u8] = b"DOCS";
const MOVE_TARGET: &[u8] = b"DOCS/MOVED.TXT";
const MOVFD_PATH: &[u8] = b"MOVFD.TXT";
const MOVFD_DOCS: &[u8] = b"DOCS/MOVFD.TXT";
const TARG_DOCS: &[u8] = b"DOCS/TARG.TXT";
const CYCLE_PATH: &[u8] = b"DOCS/X.TXT";
const SRCDIR_PATH: &[u8] = b"SRCDIR";
const SRCDIR_INNER: &[u8] = b"SRCDIR/F.TXT";
const DOCS_SRCDIR: &[u8] = b"DOCS/SRCDIR";
const DOCS_SRCDIR_INNER: &[u8] = b"DOCS/SRCDIR/F.TXT";
const SRCDIR2_PATH: &[u8] = b"SRCDIR2";
const SRCDIR2_INNER: &[u8] = b"SRCDIR2/F.TXT";
const DDA_PATH: &[u8] = b"DDA";
const DDA_INNER_DIR: &[u8] = b"DDA/INNER";
const DDA_CYCLE: &[u8] = b"DDA/INNER/X";
const NOPARENT_PATH: &[u8] = b"MISSINGD/X.TXT";
const LFN_PATH: &[u8] = b"long name.txt";
const MISSING_PATH: &[u8] = b"MISSING.TXT";
const OLDDIR_PATH: &[u8] = b"OLDDIR";
const NEWDIR_PATH: &[u8] = b"NEWDIR";
const OLDDIR_INNER: &[u8] = b"OLDDIR/F.TXT";
const NEWDIR_INNER: &[u8] = b"NEWDIR/F.TXT";
const OTHERD_PATH: &[u8] = b"OTHERD";
const OTHERD_INNER: &[u8] = b"OTHERD/F.TXT";
const EMPTYD_PATH: &[u8] = b"EMPTYD";
const EMPTYD_INNER: &[u8] = b"EMPTYD/F.TXT";
const FILE_PATH: &[u8] = b"HELLO.TXT";
const PAYLOAD: &[u8] = b"renamed by guest\n";
const BUFFER_LEN: usize = 64;

/// `path`を作り、`payload`を書いて閉じる。
fn create_with(path: &[u8], payload: &[u8]) {
    let mut file = File::create(path).unwrap();
    assert_eq!(file.write(payload), Ok(payload.len()));
    file.close().unwrap();
}

/// `path`を開いて`len` byteを読み、`buffer`へ残して閉じる。
fn read_back(path: &[u8], buffer: &mut [u8], len: usize) {
    let mut file = File::open(path).unwrap();
    assert_eq!(file.read(&mut buffer[..len]), Ok(len));
    file.close().unwrap();
}

/// `path`が存在しないことを`open`の`ENOENT`で確かめる。
fn assert_missing(path: &[u8]) {
    assert_eq!(File::open(path).err(), Some(Errno(ENOENT)));
}

minios_guest::entry!(main);

fn main(_args: Args) -> i32 {
    let mut buffer = [0u8; BUFFER_LEN];
    create_with(OLD_PATH, PAYLOAD);

    // 読み専用で開いたままrenameする。entryの位置は変わらないため、
    // 開いているfdはそのまま内容を読める。
    let mut file = File::open(OLD_PATH).unwrap();
    rename(OLD_PATH, NEW_PATH).unwrap();
    assert_eq!(file.read(&mut buffer[..PAYLOAD.len()]), Ok(PAYLOAD.len()));
    assert_eq!(&buffer[..PAYLOAD.len()], PAYLOAD);
    file.close().unwrap();

    // 旧名はENOENT、新名で開ける。同名へのrenameは成功のno-op。
    assert_missing(OLD_PATH);
    rename(NEW_PATH, NEW_PATH).unwrap();

    // errno契約：不在source、dir→file、file→dir、別dir、非8.3名。
    assert_eq!(rename(MISSING_PATH, OLD_PATH), Err(Errno(ENOENT)));
    assert_eq!(rename(DIR_PATH, FILE_PATH), Err(Errno(ENOTDIR)));
    assert_eq!(rename(NEW_PATH, DIR_PATH), Err(Errno(EISDIR)));
    assert_eq!(rename(NEW_PATH, LFN_PATH), Err(Errno(EINVAL)));

    // 既存fileへのrenameは置き換える。target側のfdは失効し、
    // target名で開くとsourceの内容が読める。失効したfdは生のまま扱い、
    // dropで閉じない。
    File::create(VICTIM_PATH).unwrap().close().unwrap();
    let victim = File::open(VICTIM_PATH).unwrap().into_raw_fd();
    rename(NEW_PATH, VICTIM_PATH).unwrap();
    assert_eq!(sys_read(victim, buffer.as_mut_ptr(), 8), EBADF);
    assert_missing(NEW_PATH);
    read_back(VICTIM_PATH, &mut buffer, PAYLOAD.len());
    assert_eq!(&buffer[..PAYLOAD.len()], PAYLOAD);

    // directoryのrename：中のfileは新しいdir名で解決できる（`..`は
    // 親clusterを指すため更新不要）。旧名はENOENTになる。
    mkdir(OLDDIR_PATH).unwrap();
    create_with(OLDDIR_INNER, PAYLOAD);
    rename(OLDDIR_PATH, NEWDIR_PATH).unwrap();
    assert_missing(OLDDIR_INNER);
    read_back(NEWDIR_INNER, &mut buffer, PAYLOAD.len());
    assert_eq!(&buffer[..PAYLOAD.len()], PAYLOAD);

    // errno契約：dir→fileはENOTDIR、file→dirはEISDIR、dir→非空dirは
    // ENOTEMPTY。
    assert_eq!(rename(NEWDIR_PATH, FILE_PATH), Err(Errno(ENOTDIR)));
    mkdir(OTHERD_PATH).unwrap();
    File::create(OTHERD_INNER).unwrap().close().unwrap();
    assert_eq!(rename(FILE_PATH, OTHERD_PATH), Err(Errno(EISDIR)));
    assert_eq!(rename(NEWDIR_PATH, OTHERD_PATH), Err(Errno(ENOTEMPTY)));

    // dir→空dirは置換。targetのentryとchainが消え、sourceが名を継ぐ。
    mkdir(EMPTYD_PATH).unwrap();
    rename(NEWDIR_PATH, EMPTYD_PATH).unwrap();
    assert_missing(NEWDIR_INNER);
    File::open(EMPTYD_INNER).unwrap().close().unwrap();

    // cross-directory move：fileをDOCSへ移す。旧名はENOENT、新名で
    // 内容を照合し、rootへ戻す。
    rename(VICTIM_PATH, MOVE_TARGET).unwrap();
    assert_missing(VICTIM_PATH);
    read_back(MOVE_TARGET, &mut buffer, PAYLOAD.len());
    assert_eq!(&buffer[..PAYLOAD.len()], PAYLOAD);
    rename(MOVE_TARGET, VICTIM_PATH).unwrap();
    assert_missing(MOVE_TARGET);

    // move中のfd追従：writable fdを開いたまま別dirへ移し、fd経由の追記が
    // 新しいdir entryへwrite-backされることをsizeで照合する。
    let mut file = File::create(MOVFD_PATH).unwrap();
    assert_eq!(file.write(PAYLOAD), Ok(PAYLOAD.len()));
    rename(MOVFD_PATH, MOVFD_DOCS).unwrap();
    assert_eq!(file.write(PAYLOAD), Ok(PAYLOAD.len()));
    file.close().unwrap();
    read_back(MOVFD_DOCS, &mut buffer, PAYLOAD.len() * 2);
    assert_missing(MOVFD_PATH);

    // cross-dir置換：DOCS内の既存fileへfileを移すと、targetを指すfdは
    // EBADFで失効し、sourceの内容が新名で読める。
    File::create(TARG_DOCS).unwrap().close().unwrap();
    let victim = File::open(TARG_DOCS).unwrap().into_raw_fd();
    rename(MOVFD_DOCS, TARG_DOCS).unwrap();
    assert_eq!(sys_read(victim, buffer.as_mut_ptr(), 8), EBADF);
    read_back(TARG_DOCS, &mut buffer, PAYLOAD.len() * 2);

    // directoryのcross-dir move：dirごとDOCSの中へ移し、中身を新pathで
    // 読んでからrootへ戻す（`..`が新parentへ更新される）。
    mkdir(SRCDIR_PATH).unwrap();
    create_with(SRCDIR_INNER, PAYLOAD);
    rename(SRCDIR_PATH, DOCS_SRCDIR).unwrap();
    assert_missing(SRCDIR_INNER);
    read_back(DOCS_SRCDIR_INNER, &mut buffer, PAYLOAD.len());
    rename(DOCS_SRCDIR, SRCDIR2_PATH).unwrap();
    File::open(SRCDIR2_INNER).unwrap().close().unwrap();

    // cycle：dirを自身や子孫の中へ移す指定はEINVAL。不在の親dirへの
    // 移動はENOENT。
    assert_eq!(rename(DIR_PATH, CYCLE_PATH), Err(Errno(EINVAL)));
    mkdir(DDA_PATH).unwrap();
    mkdir(DDA_INNER_DIR).unwrap();
    assert_eq!(rename(DDA_PATH, DDA_CYCLE), Err(Errno(EINVAL)));
    assert_eq!(rename(VICTIM_PATH, NOPARENT_PATH), Err(Errno(ENOENT)));

    // moveで作ったfileとdirを片付ける。
    unlink(TARG_DOCS).unwrap();
    unlink(SRCDIR2_INNER).unwrap();
    rmdir(SRCDIR2_PATH).unwrap();
    rmdir(DDA_INNER_DIR).unwrap();
    rmdir(DDA_PATH).unwrap();

    // 片付け：中身のfileを消してから両dirをrmdirできる。
    assert_eq!(rmdir(EMPTYD_PATH), Err(Errno(ENOTEMPTY)));
    unlink(EMPTYD_INNER).unwrap();
    rmdir(EMPTYD_PATH).unwrap();
    unlink(OTHERD_INNER).unwrap();
    rmdir(OTHERD_PATH).unwrap();

    println!("rename verified");
    42
}
