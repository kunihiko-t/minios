//! MiniOS guestの`rename`サンプル。
//!
//! fileをrenameして開いているfdが有効なまま動くこと、旧名が`ENOENT`に
//! なること、置き換えられたfileを指すfdが`EBADF`で失効すること、
//! directoryのrenameとdir/file組合せのerrno、別directoryへのmove
//! （fd追従・`..`更新・cycle拒否・cross-dir置換）を確認し、42で終了する。
//! 失敗時は70で終了する。E2Eのfile-rename検査が使う。

#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};
use minios_abi::syscall::{
    EBADF, EINVAL, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, FIRST_FILE_FD, STDOUT, SyscallNumber,
};

/// exit異常の的内code。syscall失敗や契約違反、panicで使う。
const FAILURE_EXIT: u32 = 70;
/// 正常終了code。
const SUCCESS_EXIT: u32 = 42;
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
const MESSAGE: &[u8] = b"rename verified\n";
const BUFFER_LEN: usize = 64;

/// MiniOS ABIの`create`を呼ぶ。戻り値はwritable fdか負のerrno。
fn sys_create(path: *const u8, path_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0はpath pointer兼戻り値、a1/a7は引数である。pathはU+R検証済みのstatic rangeである。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a7") SyscallNumber::Create as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`open`を呼ぶ。戻り値はread-only fdか負のerrno。
fn sys_open(path: *const u8, path_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a7") SyscallNumber::Open as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`rename`を呼ぶ。`a0`/`a1`がsource path、`a2`/`a3`が
/// target path。戻り値は0か負のerrno。
fn sys_rename(old_ptr: *const u8, old_len: usize, new_ptr: *const u8, new_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0は戻り値、a0..a3/a7は引数である。両pathはU+R検証済みのstatic rangeである。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") old_ptr as usize => returned,
            in("a1") old_len,
            in("a2") new_ptr as usize,
            in("a3") new_len,
            in("a7") SyscallNumber::Rename as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`read`を呼ぶ。戻り値は読んだbyte数、EOFは0、負はerrno。
fn sys_read(fd: usize, pointer: *mut u8, len: usize) -> isize {
    let fd_argument = fd as isize;
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd_argument => returned,
            in("a1") pointer as usize,
            in("a2") len,
            in("a7") SyscallNumber::Read as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`write`を呼ぶ。戻り値は書いたbyte数か負のerrno。
fn sys_write(fd: usize, pointer: *const u8, len: usize) -> isize {
    let fd_argument = fd as isize;
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd_argument => returned,
            in("a1") pointer as usize,
            in("a2") len,
            in("a7") SyscallNumber::Write as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`close`を呼ぶ。戻り値は0か負のerrno。
fn sys_close(fd: usize) -> isize {
    let fd_argument = fd as isize;
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") fd_argument => returned,
            in("a7") SyscallNumber::Close as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`mkdir`を呼ぶ。戻り値は0か負のerrno。
fn sys_mkdir(path: *const u8, path_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a7") SyscallNumber::Mkdir as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`rmdir`を呼ぶ。戻り値は0か負のerrno。
fn sys_rmdir(path: *const u8, path_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a7") SyscallNumber::Rmdir as usize,
            options(nostack),
        );
    }
    returned
}

/// MiniOS ABIの`unlink`を呼ぶ。戻り値は0か負のerrno。
fn sys_unlink(path: *const u8, path_len: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") path as usize => returned,
            in("a1") path_len,
            in("a7") SyscallNumber::Unlink as usize,
            options(nostack),
        );
    }
    returned
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

/// `_start`から呼ばれるRust本体。rename→fd継続→置き換え→errnoの契約を
/// 順に確かめる。
#[unsafe(no_mangle)]
extern "C" fn guest_main(_argc: usize, _argv: *const *const u8) -> ! {
    // 作成して書き込み、閉じる。
    let fd = sys_create(OLD_PATH.as_ptr(), OLD_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 読み専用で開いたままrenameする。entryの位置は変わらないため、
    // 開いているfdはそのまま内容を読める。
    let fd = sys_open(OLD_PATH.as_ptr(), OLD_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_rename(
        OLD_PATH.as_ptr(),
        OLD_PATH.len(),
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    let mut buffer = [0u8; BUFFER_LEN];
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if buffer[..PAYLOAD.len()] != *PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 旧名はENOENT、新名で開ける。同名へのrenameは成功のno-op。
    if sys_open(OLD_PATH.as_ptr(), OLD_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：不在source、dir→file、file→dir、別dir、非8.3名。
    if sys_rename(
        MISSING_PATH.as_ptr(),
        MISSING_PATH.len(),
        OLD_PATH.as_ptr(),
        OLD_PATH.len(),
    ) != ENOENT
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        DIR_PATH.as_ptr(),
        DIR_PATH.len(),
        FILE_PATH.as_ptr(),
        FILE_PATH.len(),
    ) != ENOTDIR
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        DIR_PATH.as_ptr(),
        DIR_PATH.len(),
    ) != EISDIR
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        LFN_PATH.as_ptr(),
        LFN_PATH.len(),
    ) != EINVAL
    {
        sys_exit(FAILURE_EXIT);
    }

    // 既存fileへのrenameは置き換える。target側のfdは失効し、
    // target名で開くとsourceの内容が読める。
    let fd = sys_create(VICTIM_PATH.as_ptr(), VICTIM_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let victim = sys_open(VICTIM_PATH.as_ptr(), VICTIM_PATH.len());
    if victim < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let victim = victim as usize;
    if sys_rename(
        NEW_PATH.as_ptr(),
        NEW_PATH.len(),
        VICTIM_PATH.as_ptr(),
        VICTIM_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(victim, buffer.as_mut_ptr(), 8) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(NEW_PATH.as_ptr(), NEW_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(VICTIM_PATH.as_ptr(), VICTIM_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if buffer[..PAYLOAD.len()] != *PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // directoryのrename：中のfileは新しいdir名で解決できる（`..`は
    // 親clusterを指すため更新不要）。旧名はENOENTになる。
    if sys_mkdir(OLDDIR_PATH.as_ptr(), OLDDIR_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_create(OLDDIR_INNER.as_ptr(), OLDDIR_INNER.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        OLDDIR_PATH.as_ptr(),
        OLDDIR_PATH.len(),
        NEWDIR_PATH.as_ptr(),
        NEWDIR_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(OLDDIR_INNER.as_ptr(), OLDDIR_INNER.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(NEWDIR_INNER.as_ptr(), NEWDIR_INNER.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if buffer[..PAYLOAD.len()] != *PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // errno契約：dir→fileはENOTDIR、file→dirはEISDIR、dir→非空dirは
    // ENOTEMPTY。
    if sys_rename(
        NEWDIR_PATH.as_ptr(),
        NEWDIR_PATH.len(),
        FILE_PATH.as_ptr(),
        FILE_PATH.len(),
    ) != ENOTDIR
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_mkdir(OTHERD_PATH.as_ptr(), OTHERD_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_create(OTHERD_INNER.as_ptr(), OTHERD_INNER.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        FILE_PATH.as_ptr(),
        FILE_PATH.len(),
        OTHERD_PATH.as_ptr(),
        OTHERD_PATH.len(),
    ) != EISDIR
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEWDIR_PATH.as_ptr(),
        NEWDIR_PATH.len(),
        OTHERD_PATH.as_ptr(),
        OTHERD_PATH.len(),
    ) != ENOTEMPTY
    {
        sys_exit(FAILURE_EXIT);
    }

    // dir→空dirは置換。targetのentryとchainが消え、sourceが名を継ぐ。
    if sys_mkdir(EMPTYD_PATH.as_ptr(), EMPTYD_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        NEWDIR_PATH.as_ptr(),
        NEWDIR_PATH.len(),
        EMPTYD_PATH.as_ptr(),
        EMPTYD_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(NEWDIR_INNER.as_ptr(), NEWDIR_INNER.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(EMPTYD_INNER.as_ptr(), EMPTYD_INNER.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // cross-directory move：fileをDOCSへ移す。旧名はENOENT、新名で
    // 内容を照合し、rootへ戻す。
    if sys_rename(
        VICTIM_PATH.as_ptr(),
        VICTIM_PATH.len(),
        MOVE_TARGET.as_ptr(),
        MOVE_TARGET.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(VICTIM_PATH.as_ptr(), VICTIM_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(MOVE_TARGET.as_ptr(), MOVE_TARGET.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if buffer[..PAYLOAD.len()] != *PAYLOAD {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        MOVE_TARGET.as_ptr(),
        MOVE_TARGET.len(),
        VICTIM_PATH.as_ptr(),
        VICTIM_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(MOVE_TARGET.as_ptr(), MOVE_TARGET.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }

    // move中のfd追従：writable fdを開いたまま別dirへ移し、fd経由の追記が
    // 新しいdir entryへwrite-backされることをsizeで照合する。
    let fd = sys_create(MOVFD_PATH.as_ptr(), MOVFD_PATH.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_write(fd, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        MOVFD_PATH.as_ptr(),
        MOVFD_PATH.len(),
        MOVFD_DOCS.as_ptr(),
        MOVFD_DOCS.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(MOVFD_DOCS.as_ptr(), MOVFD_DOCS.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len() * 2) != (PAYLOAD.len() * 2) as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(MOVFD_PATH.as_ptr(), MOVFD_PATH.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }

    // cross-dir置換：DOCS内の既存fileへfileを移すと、targetを指すfdは
    // EBADFで失効し、sourceの内容が新名で読める。
    let fd = sys_create(TARG_DOCS.as_ptr(), TARG_DOCS.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let victim = sys_open(TARG_DOCS.as_ptr(), TARG_DOCS.len());
    if victim < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let victim = victim as usize;
    if sys_rename(
        MOVFD_DOCS.as_ptr(),
        MOVFD_DOCS.len(),
        TARG_DOCS.as_ptr(),
        TARG_DOCS.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_read(victim, buffer.as_mut_ptr(), 8) != EBADF {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(TARG_DOCS.as_ptr(), TARG_DOCS.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len() * 2) != (PAYLOAD.len() * 2) as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // directoryのcross-dir move：dirごとDOCSの中へ移し、中身を新pathで
    // 読んでからrootへ戻す（`..`が新parentへ更新される）。
    if sys_mkdir(SRCDIR_PATH.as_ptr(), SRCDIR_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_create(SRCDIR_INNER.as_ptr(), SRCDIR_INNER.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_write(fd as usize, PAYLOAD.as_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        SRCDIR_PATH.as_ptr(),
        SRCDIR_PATH.len(),
        DOCS_SRCDIR.as_ptr(),
        DOCS_SRCDIR.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_open(SRCDIR_INNER.as_ptr(), SRCDIR_INNER.len()) != ENOENT {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(DOCS_SRCDIR_INNER.as_ptr(), DOCS_SRCDIR_INNER.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    let fd = fd as usize;
    if sys_read(fd, buffer.as_mut_ptr(), PAYLOAD.len()) != PAYLOAD.len() as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        DOCS_SRCDIR.as_ptr(),
        DOCS_SRCDIR.len(),
        SRCDIR2_PATH.as_ptr(),
        SRCDIR2_PATH.len(),
    ) != 0
    {
        sys_exit(FAILURE_EXIT);
    }
    let fd = sys_open(SRCDIR2_INNER.as_ptr(), SRCDIR2_INNER.len());
    if fd < FIRST_FILE_FD as isize {
        sys_exit(FAILURE_EXIT);
    }
    if sys_close(fd as usize) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // cycle：dirを自身や子孫の中へ移す指定はEINVAL。不在の親dirへの
    // 移動はENOENT。
    if sys_rename(
        DIR_PATH.as_ptr(),
        DIR_PATH.len(),
        CYCLE_PATH.as_ptr(),
        CYCLE_PATH.len(),
    ) != EINVAL
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_mkdir(DDA_PATH.as_ptr(), DDA_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_mkdir(DDA_INNER_DIR.as_ptr(), DDA_INNER_DIR.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        DDA_PATH.as_ptr(),
        DDA_PATH.len(),
        DDA_CYCLE.as_ptr(),
        DDA_CYCLE.len(),
    ) != EINVAL
    {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rename(
        VICTIM_PATH.as_ptr(),
        VICTIM_PATH.len(),
        NOPARENT_PATH.as_ptr(),
        NOPARENT_PATH.len(),
    ) != ENOENT
    {
        sys_exit(FAILURE_EXIT);
    }

    // moveで作ったfileとdirを片付ける。
    if sys_unlink(TARG_DOCS.as_ptr(), TARG_DOCS.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_unlink(SRCDIR2_INNER.as_ptr(), SRCDIR2_INNER.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(SRCDIR2_PATH.as_ptr(), SRCDIR2_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(DDA_INNER_DIR.as_ptr(), DDA_INNER_DIR.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(DDA_PATH.as_ptr(), DDA_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    // 片付け：中身のfileを消してから両dirをrmdirできる。
    if sys_rmdir(EMPTYD_PATH.as_ptr(), EMPTYD_PATH.len()) != ENOTEMPTY {
        sys_exit(FAILURE_EXIT);
    }
    if sys_unlink(EMPTYD_INNER.as_ptr(), EMPTYD_INNER.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(EMPTYD_PATH.as_ptr(), EMPTYD_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_unlink(OTHERD_INNER.as_ptr(), OTHERD_INNER.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }
    if sys_rmdir(OTHERD_PATH.as_ptr(), OTHERD_PATH.len()) != 0 {
        sys_exit(FAILURE_EXIT);
    }

    if sys_write(STDOUT, MESSAGE.as_ptr(), MESSAGE.len()) != MESSAGE.len() as isize {
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
