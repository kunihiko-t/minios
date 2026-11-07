//! processの起動、待機、終了と、時刻、pipe、fdの複製とclose-on-exec。

use minios_abi::syscall::{
    EINVAL, EIO, F_GETFD, F_SETFD, FD_CLOEXEC, PIPE_OUT_LEN, PROCINFO_LEN, ProcInfo, SPAWN_MAX_ARGC,
};

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

/// `pid`のprocessへ終了を要求する。回収されたprocessは`wait`から
/// `EXIT_CODE_KILLED` (75) として回収できる。
pub fn kill(pid: Pid) -> Result<()> {
    check(sys::sys_kill(pid)).map(|_| ())
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

/// process tableのprocessをpidの小さい順に返す。1件ごとに`procinfo`を
/// 1回呼ぶため、一覧の途中で増減したprocessは反映されたりされなかったりする。
pub fn processes() -> Processes {
    Processes {
        index: 0,
        done: false,
    }
}

/// `processes`が返すiterator。末尾かerrnoで終わる。
pub struct Processes {
    index: usize,
    done: bool,
}

impl Iterator for Processes {
    type Item = Result<ProcInfo>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut out = [0u8; PROCINFO_LEN];
        let returned = sys::sys_procinfo(self.index, out.as_mut_ptr());
        self.index += 1;
        let item = match check(returned) {
            Ok(0) => None,
            Ok(PROCINFO_LEN) => return Some(Ok(ProcInfo::from_le_bytes(out))),
            Ok(_) => Some(Err(Errno(EIO))),
            Err(errno) => Some(Err(errno)),
        };
        self.done = true;
        item
    }
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
/// 複製先の`newfd`はclose-on-execの印を持たない。
pub fn dup2(oldfd: usize, newfd: usize) -> Result<usize> {
    check(sys::sys_dup2(oldfd, newfd))
}

/// `fd`のclose-on-execの印を設定する。印を立てたfdは`spawn`したchildへ
/// 継承されず、`exec`の成功時に閉じられる。所有しない生のfd（shellが
/// 退避したconsoleなど）にも使える。
pub fn set_cloexec(fd: usize, on: bool) -> Result<()> {
    let flags = if on { FD_CLOEXEC } else { 0 };
    check(sys::sys_fcntl(fd, F_SETFD, flags)).map(|_| ())
}

/// `fd`にclose-on-execの印が立っていれば`true`。
pub fn cloexec(fd: usize) -> Result<bool> {
    check(sys::sys_fcntl(fd, F_GETFD, 0)).map(|flags| flags & FD_CLOEXEC != 0)
}
