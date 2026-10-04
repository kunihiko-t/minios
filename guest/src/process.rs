//! processの起動、待機、終了と、時刻、pipe、fdの複製。

use minios_abi::syscall::{EINVAL, PIPE_OUT_LEN, SPAWN_MAX_ARGC};

use crate::{Errno, Result, check, fs::File, sys};

/// process tableが採番するpid。
pub type Pid = usize;

/// `path`のELFをchild processとして起動する。childはfd tableのsnapshotを
/// 引き継ぐ。`args`が空ならchildの`argv`はpathのbasenameだけ、空でなければ
/// `args`がそのまま`argv`になる（`args[0]`がprocess名）。
pub fn spawn(path: impl AsRef<[u8]>, args: &[&[u8]]) -> Result<Pid> {
    if args.len() > SPAWN_MAX_ARGC {
        return Err(Errno(EINVAL));
    }
    let mut argv = [[0u64; 2]; SPAWN_MAX_ARGC];
    for (entry, arg) in argv.iter_mut().zip(args) {
        *entry = [arg.as_ptr() as u64, arg.len() as u64];
    }
    let path = path.as_ref();
    check(sys::sys_spawn(
        path.as_ptr(),
        path.len(),
        argv.as_ptr(),
        args.len(),
    ))
}

/// `pid`の終了を待ち、終了codeを返す。
pub fn wait(pid: Pid) -> Result<i32> {
    check(sys::sys_waitpid(pid)).map(|code| code as i32)
}

/// 呼び出しprocessのimageを`path`のELFで置き換える。成功時は戻らないため、
/// 戻り値は常に失敗のerrnoである。
pub fn exec(path: impl AsRef<[u8]>) -> Errno {
    let path = path.as_ref();
    Errno(sys::sys_exec(path.as_ptr(), path.len()))
}

/// 呼び出しprocessのpid。
pub fn getpid() -> Pid {
    sys::sys_getpid() as Pid
}

/// `code`で終了する。戻らない。
pub fn exit(code: i32) -> ! {
    sys::sys_exit(code as u32)
}

/// `millis`以上待つ。0はyieldと同じ。
pub fn sleep_ms(millis: usize) {
    let _ = sys::sys_sleep(millis);
}

/// 残りのtime sliceを手放す。
pub fn yield_now() {
    let _ = sys::sys_yield();
}

/// boot以降の経過millisecond（10 ms単位）。
pub fn clock_ms() -> u64 {
    sys::sys_clock() as u64
}

/// pipeを作り、`(read端, write端)`を返す。両端は`spawn`したchildへ継承される。
pub fn pipe() -> Result<(File, File)> {
    let mut fds = [0u32; 2];
    if check(sys::sys_pipe(&mut fds))? != PIPE_OUT_LEN {
        return Err(Errno(minios_abi::syscall::EIO));
    }
    Ok((
        File::from_raw_fd(fds[0] as usize),
        File::from_raw_fd(fds[1] as usize),
    ))
}

/// `newfd`を閉じてから`oldfd`と同じentryを指させ、`newfd`を返す。
pub fn dup2(oldfd: usize, newfd: usize) -> Result<usize> {
    check(sys::sys_dup2(oldfd, newfd))
}
