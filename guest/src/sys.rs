//! MiniOS ABI (`minios_abi::syscall::SyscallNumber`) のsyscall wrapper。
//!
//! 引数はkernelへregisterで渡す値をそのまま受ける。pointerはraw pointerで
//! 受け、検証はkernelに任せる。testが意図的に不正pointerを渡して`EFAULT`
//! を確かめられるようにするためである。戻り値は非負の結果か負のerrno。

use core::arch::asm;
use minios_abi::syscall::SyscallNumber;

/// `a7=number`、`a0`..`a3`を引数として`ecall`し、`a0`の戻り値を返す。
/// 使わない引数registerには0を渡す。
fn ecall(number: SyscallNumber, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let returned: isize;
    // Safety: ecallはkernelへtrapし、全registerはuser trap contextで保存復元される。
    // a0は引数兼戻り値、a1..a3/a7は引数である。user pointerはkernelが
    // U-mode rangeとpermissionを検証してから触れる。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") a0 => returned,
            in("a1") a1,
            in("a2") a2,
            in("a3") a3,
            in("a7") number as usize,
            options(nostack),
        );
    }
    returned
}

/// `write`を呼ぶ。戻り値は書いたbyte数か負のerrno。
pub fn sys_write(fd: usize, pointer: *const u8, len: usize) -> isize {
    ecall(SyscallNumber::Write, fd, pointer as usize, len, 0)
}

/// `exit`。kernelはこの呼び出しの後guestへ戻らない。
pub fn sys_exit(code: u32) -> ! {
    // Safety: exitのecallはresumeしない契約のため、noreturnでよい。
    unsafe {
        asm!(
            "ecall",
            in("a0") code as usize,
            in("a7") SyscallNumber::Exit as usize,
            options(noreturn),
        );
    }
}

/// `read`を呼ぶ。戻り値は読んだbyte数、EOFは0、負はerrno。
pub fn sys_read(fd: usize, pointer: *mut u8, len: usize) -> isize {
    ecall(SyscallNumber::Read, fd, pointer as usize, len, 0)
}

/// `read_file`を呼ぶ。pathのfile内容を`buf`へ最大`buf_len` byte読む。
pub fn sys_read_file(path: *const u8, path_len: usize, buf: *mut u8, buf_len: usize) -> isize {
    ecall(
        SyscallNumber::ReadFile,
        path as usize,
        path_len,
        buf as usize,
        buf_len,
    )
}

/// `open`を呼ぶ。戻り値はread-only fdか負のerrno。
pub fn sys_open(path: *const u8, path_len: usize) -> isize {
    ecall(SyscallNumber::Open, path as usize, path_len, 0, 0)
}

/// `close`を呼ぶ。戻り値は0か負のerrno。
pub fn sys_close(fd: usize) -> isize {
    ecall(SyscallNumber::Close, fd, 0, 0, 0)
}

/// `create`を呼ぶ。戻り値はwritable fdか負のerrno。
pub fn sys_create(path: *const u8, path_len: usize) -> isize {
    ecall(SyscallNumber::Create, path as usize, path_len, 0, 0)
}

/// `unlink`を呼ぶ。戻り値は0か負のerrno。
pub fn sys_unlink(path: *const u8, path_len: usize) -> isize {
    ecall(SyscallNumber::Unlink, path as usize, path_len, 0, 0)
}

/// `lseek`を呼ぶ。`offset`は符号付き。戻り値は新しいoffsetか負のerrno。
pub fn sys_lseek(fd: usize, offset: isize, whence: usize) -> isize {
    ecall(SyscallNumber::Lseek, fd, offset as usize, whence, 0)
}

/// `pread`を呼ぶ。fdのoffsetを動かさず`offset`から読む。
pub fn sys_pread(fd: usize, pointer: *mut u8, len: usize, offset: u64) -> isize {
    ecall(
        SyscallNumber::Pread,
        fd,
        pointer as usize,
        len,
        offset as usize,
    )
}

/// `pwrite`を呼ぶ。fdのoffsetを動かさず`offset`へ書く。
pub fn sys_pwrite(fd: usize, pointer: *const u8, len: usize, offset: u64) -> isize {
    ecall(
        SyscallNumber::Pwrite,
        fd,
        pointer as usize,
        len,
        offset as usize,
    )
}

/// `rename`を呼ぶ。戻り値は0か負のerrno。
pub fn sys_rename(old_ptr: *const u8, old_len: usize, new_ptr: *const u8, new_len: usize) -> isize {
    ecall(
        SyscallNumber::Rename,
        old_ptr as usize,
        old_len,
        new_ptr as usize,
        new_len,
    )
}

/// `mkdir`を呼ぶ。戻り値は0か負のerrno。
pub fn sys_mkdir(path: *const u8, path_len: usize) -> isize {
    ecall(SyscallNumber::Mkdir, path as usize, path_len, 0, 0)
}

/// `rmdir`を呼ぶ。戻り値は0か負のerrno。
pub fn sys_rmdir(path: *const u8, path_len: usize) -> isize {
    ecall(SyscallNumber::Rmdir, path as usize, path_len, 0, 0)
}

/// `getpid`を呼ぶ。戻り値は呼び出しprocessのpid。
pub fn sys_getpid() -> isize {
    ecall(SyscallNumber::Getpid, 0, 0, 0, 0)
}

/// `spawn`を呼ぶ。childはcallerのfd tableのsnapshotを引き継ぐ。
/// 戻り値はchildのpidか負のerrno。
pub fn sys_spawn(path: *const u8, path_len: usize) -> isize {
    ecall(SyscallNumber::Spawn, path as usize, path_len, 0, 0)
}

/// `waitpid`を呼ぶ。戻り値は対象processの終了codeか負のerrno。
pub fn sys_waitpid(pid: usize) -> isize {
    ecall(SyscallNumber::Waitpid, pid, 0, 0, 0)
}

/// `stat`を呼ぶ。`out`へ8 byteの`Stat`をLEで書き込む。
pub fn sys_stat(path: *const u8, path_len: usize, out: *mut u8) -> isize {
    ecall(
        SyscallNumber::Stat,
        path as usize,
        path_len,
        out as usize,
        0,
    )
}

/// `fstat`を呼ぶ。`out`へ8 byteの`Stat`をLEで書き込む。
pub fn sys_fstat(fd: usize, out: *mut u8) -> isize {
    ecall(SyscallNumber::Fstat, fd, out as usize, 0, 0)
}

/// `readdir`を呼ぶ。directoryの`index`番目のentryを`out`へ`DirEnt`として
/// 書き込む。indexが末尾を越えれば0を返す。
pub fn sys_readdir(path: *const u8, path_len: usize, index: usize, out: *mut u8) -> isize {
    ecall(
        SyscallNumber::Readdir,
        path as usize,
        path_len,
        index,
        out as usize,
    )
}

/// `exec`を呼ぶ。成功時は戻らず、失敗時のみ負のerrnoを返す。
pub fn sys_exec(path: *const u8, path_len: usize) -> isize {
    ecall(SyscallNumber::Exec, path as usize, path_len, 0, 0)
}

/// `pipe`を呼ぶ。`out`へ`[read_fd, write_fd]`を8 byteで書き込む。
/// 戻り値は書き込んだbyte数(8)か負のerrno。
pub fn sys_pipe(out: *mut [u32; 2]) -> isize {
    ecall(SyscallNumber::Pipe, out as usize, 0, 0, 0)
}
