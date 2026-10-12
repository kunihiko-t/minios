use crate::{
    user::{
        context::UserContext,
        memory::{check_user_writable_range, copy_from_user, copy_to_user},
    },
    vm::{AddressSpace, FrameStore},
};
use alloc::vec::Vec;
use minios_abi::{
    control::FrameKind,
    manifest::ARG_MAX_LEN,
    syscall::{
        DIRENT_LEN, EAGAIN, EBADF, EFAULT, EINVAL, ENOMEM, ENOSYS, ESPIPE, MAX_PATH_LEN,
        MAX_READ_LEN, MAX_WRITE_LEN, PIPE_OUT_LEN, SOCKADDR_LEN, SPAWN_ARG_LEN, SPAWN_MAX_ARGC,
        STAT_KIND_CONSOLE, STAT_LEN, Stat, SyscallNumber,
    },
};

use super::{ConsoleFd, ControlSink, ControlSource, FD_TABLE_LEN, SyscallFlow};

/// `EAGAIN`なら`sepc`をecallへ戻して`Blocked`を返し、入力到着やI/O完了の
/// 再試行が同じsyscallをやり直す。guestへ`EAGAIN`自体が返ることはない。
/// pipeやdisk I/Oの完了待ちでmark済みのcallerが使う。それ以外のerrnoは
/// そのまま`a0`へ書いて`Resume`を返す。
fn errno_flow<E, SE>(context: &mut UserContext, errno: isize) -> SyscallFlow<E, SE> {
    if errno == EAGAIN {
        context.set_sepc(context.sepc() - 4);
        SyscallFlow::Blocked
    } else {
        context.set_register(10, errno as usize);
        SyscallFlow::Resume
    }
}

/// `a7`のsystem call番号に従って`context`を処理する。
///
/// 各syscallはguest pointerをRust参照として解することなく引数を検証し、
/// storage・process・pipeの操作は`source`へ、console出力は`sink`へ委ねて、
/// 戻り値かerrnoを`a0`へ書き込む。未知の番号は`ENOSYS`である。
/// `write`は1回の検証付きcopyの後、console出力を指すfdなら1回の`sink.frame`へ、
/// fileとpipeのfdなら`source.write_fd`へ流し、4,096 byteを超える長さは拒否する。
/// `read`は書き込み検証を通してから、console入力を指すfdなら1回の
/// `source.read_stdin`で、fileとpipeのfdなら`source.read_fd`で`read_scratch`へ
/// 受信する。どのfdがconsoleを指すかは`source.console_fd`が決め、fd番号では
/// 決めない。stdinの未到着（`Ok(None)`）、pipe待ち（`EAGAIN`）、`waitpid`の
/// 対象がliveな場合は`Blocked`を返し、`sepc`をecallへ戻して再開時の再実行に
/// 委ねる。`socket`/`sendto`/`recvfrom`はUDP socket tableを`source`経由で
/// 操作し、`recvfrom`の未受信は`Blocked`で再実行、`sendto`は送信byte数を
/// `a0`へ返す。userへのcopyは呼び出し側の`ReadComplete`/`RecvComplete`
/// 処理へ委ねる。scratchは呼び出し側が1個だけ持ち、多重frameへ4 KiBを
/// 複製しない。
pub fn dispatch_syscall<M: FrameStore, S: ControlSink, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    sink: &mut S,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<S::Error, R::Error> {
    let number = context.register(17);
    if number == SyscallNumber::Write as usize {
        dispatch_write(context, space, memory, sink, source)
    } else if number == SyscallNumber::Read as usize {
        dispatch_read(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::ReadFile as usize {
        dispatch_read_file(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Open as usize {
        dispatch_open(context, space, memory, source, false)
    } else if number == SyscallNumber::Create as usize {
        dispatch_open(context, space, memory, source, true)
    } else if number == SyscallNumber::Unlink as usize {
        dispatch_path_op(context, space, memory, source, |source, path| {
            source.unlink(path)
        })
    } else if number == SyscallNumber::Mkdir as usize {
        dispatch_path_op(context, space, memory, source, |source, path| {
            source.make_dir(path)
        })
    } else if number == SyscallNumber::Rmdir as usize {
        dispatch_path_op(context, space, memory, source, |source, path| {
            source.remove_dir(path)
        })
    } else if number == SyscallNumber::Lseek as usize {
        dispatch_lseek(context, source)
    } else if number == SyscallNumber::Pread as usize {
        dispatch_pread(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Pwrite as usize {
        dispatch_pwrite(context, space, memory, source)
    } else if number == SyscallNumber::Rename as usize {
        dispatch_rename(context, space, memory, source)
    } else if number == SyscallNumber::Close as usize {
        dispatch_close(context, source)
    } else if number == SyscallNumber::Getpid as usize {
        dispatch_getpid(context, source)
    } else if number == SyscallNumber::Spawn as usize {
        dispatch_spawn(context, space, memory, source)
    } else if number == SyscallNumber::Waitpid as usize {
        dispatch_waitpid(context, source)
    } else if number == SyscallNumber::Stat as usize {
        dispatch_stat(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Fstat as usize {
        dispatch_fstat(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Readdir as usize {
        dispatch_readdir(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Exec as usize {
        dispatch_exec(context, space, memory, source)
    } else if number == SyscallNumber::Pipe as usize {
        dispatch_pipe(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Sbrk as usize {
        let result = source.sbrk(context.register(10) as isize);
        context.set_register(
            10,
            result.map_or_else(|errno| errno as usize, |old| old as usize),
        );
        SyscallFlow::Resume
    } else if number == SyscallNumber::Clock as usize {
        context.set_register(10, source.clock() as usize);
        SyscallFlow::Resume
    } else if number == SyscallNumber::Sleep as usize {
        // `sleep(0)`はmarkせずyieldと同じ経路へ流す。
        let millis = context.register(10);
        match if millis == 0 {
            Ok(())
        } else {
            source.sleep(millis)
        } {
            Ok(()) => {
                context.set_register(10, 0);
                SyscallFlow::Yield
            }
            Err(errno) => errno_flow(context, errno),
        }
    } else if number == SyscallNumber::Yield as usize {
        context.set_register(10, 0);
        SyscallFlow::Yield
    } else if number == SyscallNumber::Dup2 as usize {
        dispatch_dup2(context, source)
    } else if number == SyscallNumber::Kill as usize {
        dispatch_kill(context, source)
    } else if number == SyscallNumber::Socket as usize {
        match source.socket() {
            Ok(sock) => {
                context.set_register(10, sock);
                SyscallFlow::Resume
            }
            Err(errno) => errno_flow(context, errno),
        }
    } else if number == SyscallNumber::SendTo as usize {
        dispatch_send_to(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::RecvFrom as usize {
        dispatch_recv_from(context, space, memory, source, read_scratch)
    } else if number == SyscallNumber::Exit as usize {
        SyscallFlow::Exit(context.register(10) as u32)
    } else {
        context.set_register(10, ENOSYS as usize);
        SyscallFlow::Resume
    }
}

fn dispatch_read<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let fd = context.register(10);
    let console = source.console_fd(fd);
    let readable = match console {
        Some(ConsoleFd::Stdin) => true,
        Some(ConsoleFd::Stdout | ConsoleFd::Stderr) => false,
        None => fd < FD_TABLE_LEN,
    };
    if !readable {
        context.set_register(10, EBADF as usize);
        return SyscallFlow::Resume;
    }
    let len = context.register(12);
    if len > MAX_READ_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    if len == 0 {
        context.set_register(10, 0);
        return SyscallFlow::Resume;
    }
    let start = context.register(11) as u64;
    // stdinの消費は不可逆なため、sourceへ触れる前にEFAULTを確定させる。
    if check_user_writable_range(space, memory, start, len).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    if console.is_some() {
        // `Ok(None)`は入力未到着である。`sepc`はclassifyがecallの次へ進めて
        // あるため、4 byte戻して再開時に同じecallをやり直す。
        return match source.read_stdin(&mut read_scratch[..len]) {
            Ok(Some(count)) => SyscallFlow::ReadComplete { start, len: count },
            Ok(None) => {
                context.set_sepc(context.sepc() - 4);
                SyscallFlow::Blocked
            }
            Err(error) => SyscallFlow::SourceFatal(error),
        };
    }
    match source.read_fd(fd, &mut read_scratch[..len]) {
        Ok(count) => SyscallFlow::ReadComplete {
            start,
            len: count.min(len),
        },
        // `EAGAIN`はcontrol層がcallerをpipe待ちやI/O完了待ちへmark済みの
        // signalであり、stdinの`Ok(None)`と同じくecallへ戻して再実行する。
        Err(errno) => errno_flow(context, errno),
    }
}

/// `a0/a1`が指すuser memoryのpathを`path_buf`へ検証付きcopyし、長さを
/// 返す。長さの範囲違反は`EINVAL`、user range外は`EFAULT`を`a0`へ書いて
/// `None`を返す。fd割り当てやdir entry変更のside effectより先に
/// EFAULT/EINVALを確定する規約を、pathを1個取るsyscall（open/create/
/// unlink/mkdir/rmdir/spawn/exec/stat/readdir）で共有する。
fn copy_user_path<M: FrameStore>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    path_buf: &mut [u8; MAX_PATH_LEN],
) -> Option<usize> {
    copy_user_path_at(context, space, memory, 10, 11, path_buf)
}

/// `ptr_register`/`len_register`が指すuser memoryのpathを`path_buf`へ
/// copyする。失敗時は`a0`へerrnoを書いて`None`を返す。2つのpathを取る
/// `rename`ではsourceに`a0`/`a1`、targetに`a2`/`a3`を指定する。
fn copy_user_path_at<M: FrameStore>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    ptr_register: usize,
    len_register: usize,
    path_buf: &mut [u8; MAX_PATH_LEN],
) -> Option<usize> {
    let path_len = context.register(len_register);
    if path_len == 0 || path_len > MAX_PATH_LEN {
        context.set_register(10, EINVAL as usize);
        return None;
    }
    if copy_from_user(
        space,
        memory,
        context.register(ptr_register) as u64,
        &mut path_buf[..path_len],
    )
    .is_err()
    {
        context.set_register(10, EFAULT as usize);
        return None;
    }
    Some(path_len)
}

/// `open`と`create` (`a0=path_ptr, a1=path_len`)。検証規約は`read_file`と
/// 同じで、fd割り当てやdir entry作成のside effectより先にpathの
/// EFAULT/EINVALを確定する。`create`はfileをwritableに開く。
/// 成功時は`a0`へfdを返す。
fn dispatch_open<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    create: bool,
) -> SyscallFlow<E, R::Error> {
    let mut path_buf = [0u8; MAX_PATH_LEN];
    let Some(path_len) = copy_user_path(context, space, memory, &mut path_buf) else {
        return SyscallFlow::Resume;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };
    let result = if create {
        source.create_file(path)
    } else {
        source.open_file(path)
    };
    match result {
        Ok(fd) => context.set_register(10, fd),
        Err(errno) => return errno_flow(context, errno),
    }
    SyscallFlow::Resume
}

/// `unlink`/`mkdir`/`rmdir` (`a0=path_ptr, a1=path_len`)。検証規約は
/// `open`と同じ。成功時は`a0`へ0を返す。pathを取り結果が`()`の操作は
/// この1経路へ集約する。
fn dispatch_path_op<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    op: impl FnOnce(&mut R, &str) -> Result<(), isize>,
) -> SyscallFlow<E, R::Error> {
    let mut path_buf = [0u8; MAX_PATH_LEN];
    let Some(path_len) = copy_user_path(context, space, memory, &mut path_buf) else {
        return SyscallFlow::Resume;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };
    match op(source, path) {
        Ok(()) => context.set_register(10, 0),
        Err(errno) => return errno_flow(context, errno),
    }
    SyscallFlow::Resume
}

/// `rename` (`a0=old_ptr, a1=old_len, a2=new_ptr, a3=new_len`)。
/// 両pathのEFAULT/EINVALをstorageのside effectより先に確定するため、
/// sourceへ渡す前に2つともcopyとUTF-8検証を済ませる。成功時は`a0`へ0を
/// 返す。
fn dispatch_rename<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let mut old_buf = [0u8; MAX_PATH_LEN];
    let Some(old_len) = copy_user_path_at(context, space, memory, 10, 11, &mut old_buf) else {
        return SyscallFlow::Resume;
    };
    let mut new_buf = [0u8; MAX_PATH_LEN];
    let Some(new_len) = copy_user_path_at(context, space, memory, 12, 13, &mut new_buf) else {
        return SyscallFlow::Resume;
    };
    let (Ok(old_path), Ok(new_path)) = (
        core::str::from_utf8(&old_buf[..old_len]),
        core::str::from_utf8(&new_buf[..new_len]),
    ) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };
    match source.rename(old_path, new_path) {
        Ok(()) => context.set_register(10, 0),
        Err(errno) => return errno_flow(context, errno),
    }
    SyscallFlow::Resume
}

/// `getpid`（引数なし）。呼び出しprocessのpidを`a0`へ返す。
/// storageやuser memoryへ触れない。
fn dispatch_getpid<E, R: ControlSource>(
    context: &mut UserContext,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    context.set_register(10, source.getpid() as usize);
    SyscallFlow::Resume
}

/// `spawn` (`a0=path_ptr, a1=path_len, a2=argv_ptr, a3=argc`)。path検証は
/// `open`と同じ規約で、argvも`copy_spawn_argv`でkernelへcopyしてから
/// sourceへ渡す。process生成のside effectより先にEFAULT/EINVALを確定
/// するため、失敗したspawnはpidもframeも消費しない。成功時は`a0`へ
/// childのpidを返す。
fn dispatch_spawn<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let mut path_buf = [0u8; MAX_PATH_LEN];
    let Some(path_len) = copy_user_path(context, space, memory, &mut path_buf) else {
        return SyscallFlow::Resume;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };
    let mut strings = Vec::new();
    let mut ends = [0usize; SPAWN_MAX_ARGC];
    let argc = match copy_spawn_argv(context, space, memory, &mut strings, &mut ends) {
        Ok(argc) => argc,
        Err(errno) => return errno_flow(context, errno),
    };
    let mut argv = [""; SPAWN_MAX_ARGC];
    let mut start = 0;
    for (slot, &end) in argv.iter_mut().zip(&ends[..argc]) {
        // copy_spawn_argvが各文字列のUTF-8を検証済みである。
        *slot = core::str::from_utf8(&strings[start..end]).unwrap_or_default();
        start = end;
    }
    match source.spawn(path, &argv[..argc]) {
        Ok(pid) => context.set_register(10, pid),
        Err(errno) => return errno_flow(context, errno),
    }
    SyscallFlow::Resume
}

/// `spawn`の`a2`/`a3`が指すargvを`strings`へ詰めてcopyし、argcを返す。
/// `ends[i]`はi番目の文字列の`strings`内の終端offsetである。
/// `a3`=0は`a2`を読まない。`a3`が`SPAWN_MAX_ARGC`超、文字列が
/// `ARG_MAX_LEN`超・NUL含み・非UTF-8なら`EINVAL`、entry列か文字列が
/// 読めなければ`EFAULT`。entryは先頭から順に検査する。
fn copy_spawn_argv<M: FrameStore>(
    context: &UserContext,
    space: &AddressSpace,
    memory: &M,
    strings: &mut Vec<u8>,
    ends: &mut [usize; SPAWN_MAX_ARGC],
) -> Result<usize, isize> {
    let argc = context.register(13);
    if argc > SPAWN_MAX_ARGC {
        return Err(EINVAL);
    }
    let mut table = [0u8; SPAWN_MAX_ARGC * SPAWN_ARG_LEN];
    let table = &mut table[..argc * SPAWN_ARG_LEN];
    copy_from_user(space, memory, context.register(12) as u64, table).map_err(|_| EFAULT)?;
    // 最大長で一度だけ確保し、以降のresizeで再確保しない。
    strings
        .try_reserve_exact(argc * ARG_MAX_LEN)
        .map_err(|_| ENOMEM)?;
    let (entries, _) = table.as_chunks::<SPAWN_ARG_LEN>();
    for (entry, end) in entries.iter().zip(ends.iter_mut()) {
        let (pointer, len) = entry.split_at(8);
        let pointer = u64::from_le_bytes(pointer.try_into().unwrap_or_default());
        let len = u64::from_le_bytes(len.try_into().unwrap_or_default());
        if len > ARG_MAX_LEN as u64 {
            return Err(EINVAL);
        }
        let start = strings.len();
        strings.resize(start + len as usize, 0);
        let text = &mut strings[start..];
        copy_from_user(space, memory, pointer, text).map_err(|_| EFAULT)?;
        if text.contains(&0) || core::str::from_utf8(text).is_err() {
            return Err(EINVAL);
        }
        *end = strings.len();
    }
    Ok(argc)
}

/// `exec` (`a0=path_ptr, a1=path_len`)。path検証は`spawn`と同じ規約。
/// 成功時はsourceが返す新imageの初期contextをtrap slotへ上書きして
/// `Exec`を返す。`sepc`のecallへは戻らず、kernel復帰後にprocessは
/// 新しいsatpとcontextでdispatchされる。旧imageはcaller satpが有効な
/// 間は解放できないため、source側が`Process::retired_image`へ退避済み
/// である。失敗時はerrnoを`a0`へ書いてResumeし、旧imageのまま続く。
fn dispatch_exec<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let mut path_buf = [0u8; MAX_PATH_LEN];
    let Some(path_len) = copy_user_path(context, space, memory, &mut path_buf) else {
        return SyscallFlow::Resume;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };
    match source.exec(path) {
        Ok(initial) => {
            *context = initial;
            SyscallFlow::Exec
        }
        Err(errno) => errno_flow(context, errno),
    }
}

/// `waitpid` (`a0=pid`)。`Ok(Some(code))`は終了codeを`a0`へ、
/// `Ok(None)`は対象がliveなので`read`と同じく`sepc`をecallへ戻して
/// `Blocked`を返し、対象終了後の再実行でcodeを回収する。
/// `Err(errno)`は`ECHILD`/`EINVAL`等をそのまま`a0`へ返す。
fn dispatch_waitpid<E, R: ControlSource>(
    context: &mut UserContext,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    match source.waitpid(context.register(10)) {
        Ok(Some(code)) => {
            context.set_register(10, code as usize);
            SyscallFlow::Resume
        }
        Ok(None) => {
            context.set_sepc(context.sepc() - 4);
            SyscallFlow::Blocked
        }
        Err(errno) => errno_flow(context, errno),
    }
}

/// `kill` (`a0=pid`)。`Ok`は0を、`Err(errno)`は`ECHILD`/`EINVAL`等を
/// そのまま`a0`へ返し、呼び出しprocessはblockせず継続する。
/// 対象の回収自体はrun loop側が行う。
fn dispatch_kill<E, R: ControlSource>(
    context: &mut UserContext,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    match source.kill(context.register(10)) {
        Ok(()) => {
            context.set_register(10, 0);
            SyscallFlow::Resume
        }
        Err(errno) => errno_flow(context, errno),
    }
}

/// `stat` (`a0=path_ptr, a1=path_len, a2=out_ptr`)。pathとoutの検証を
/// sourceのside effect前に確定し、成功時はserializeした`Stat`をscratch
/// 経由で`out`へcopyする。`read`と同じReadCompleteのcopy-outを使う。
fn dispatch_stat<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let mut path_buf = [0u8; MAX_PATH_LEN];
    let Some(path_len) = copy_user_path(context, space, memory, &mut path_buf) else {
        return SyscallFlow::Resume;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };
    let start = context.register(12) as u64;
    if check_user_writable_range(space, memory, start, STAT_LEN).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    match source.stat(path) {
        Ok(stat) => {
            read_scratch[..STAT_LEN].copy_from_slice(&stat.to_le_bytes());
            SyscallFlow::ReadComplete {
                start,
                len: STAT_LEN,
            }
        }
        Err(errno) => errno_flow(context, errno),
    }
}

/// `fstat` (`a0=fd, a1=out_ptr`)。consoleを指すfdはここで`size`=0の
/// `STAT_KIND_CONSOLE`を返し、それ以外のfd（fileとpipe）はsourceへ委譲する。
/// `FD_TABLE_LEN`以上のfdはここで、未割当fdはsourceが`EBADF`を返す。
/// copy-outは`stat`と同じReadComplete経路。
fn dispatch_fstat<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let fd = context.register(10);
    let console = source.console_fd(fd);
    if console.is_none() && fd >= FD_TABLE_LEN {
        context.set_register(10, EBADF as usize);
        return SyscallFlow::Resume;
    }
    let start = context.register(11) as u64;
    if check_user_writable_range(space, memory, start, STAT_LEN).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    let result = match console {
        Some(_) => Ok(Stat {
            size: 0,
            kind: STAT_KIND_CONSOLE,
        }),
        None => source.fstat(fd),
    };
    match result {
        Ok(stat) => {
            read_scratch[..STAT_LEN].copy_from_slice(&stat.to_le_bytes());
            SyscallFlow::ReadComplete {
                start,
                len: STAT_LEN,
            }
        }
        Err(errno) => errno_flow(context, errno),
    }
}

/// `readdir` (`a0=path_ptr, a1=path_len, a2=index, a3=out_ptr`)。
/// `a1`=0はrootを指す特別規約で、それ以外のpathは`stat`と同じ検証。
/// `Some`ならserializeして`ReadComplete`でcopy-out、`None`は`a0`=0
/// のEOF規約。out rangeの検証はsourceのside effectより先に確定する。
fn dispatch_readdir<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let mut path_buf = [0u8; MAX_PATH_LEN];
    let path_len = context.register(11);
    let path = if path_len == 0 {
        // 空pathはrootを指す`readdir`固有の規約。pointerは参照しない。
        ""
    } else {
        let Some(len) = copy_user_path(context, space, memory, &mut path_buf) else {
            return SyscallFlow::Resume;
        };
        let Ok(path) = core::str::from_utf8(&path_buf[..len]) else {
            context.set_register(10, EINVAL as usize);
            return SyscallFlow::Resume;
        };
        path
    };
    let index = context.register(12);
    let start = context.register(13) as u64;
    if check_user_writable_range(space, memory, start, DIRENT_LEN).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    match source.readdir(path, index) {
        Ok(Some(dirent)) => {
            read_scratch[..DIRENT_LEN].copy_from_slice(&dirent.to_le_bytes());
            SyscallFlow::ReadComplete {
                start,
                len: DIRENT_LEN,
            }
        }
        Ok(None) => {
            context.set_register(10, 0);
            SyscallFlow::Resume
        }
        Err(errno) => errno_flow(context, errno),
    }
}

/// `lseek`/`pread`/`pwrite`の前検査。範囲外のfdは`EBADF`、console
/// entryはpipe端と同じく位置を持たないため`ESPIPE`を返す。
fn unseekable_fd<R: ControlSource>(source: &mut R, fd: usize) -> Option<isize> {
    if fd >= FD_TABLE_LEN {
        Some(EBADF)
    } else if source.console_fd(fd).is_some() {
        Some(ESPIPE)
    } else {
        None
    }
}

/// `lseek` (`a0=fd, a1=offset(isize), a2=whence`)。file fdのみが対象で、
/// console entryは`ESPIPE`、未割当fdは`EBADF`を返す。新offsetを`a0`へ返す。
fn dispatch_lseek<E, R: ControlSource>(
    context: &mut UserContext,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let fd = context.register(10);
    if let Some(errno) = unseekable_fd(source, fd) {
        context.set_register(10, errno as usize);
        return SyscallFlow::Resume;
    }
    match source.seek_fd(fd, context.register(11) as isize, context.register(12)) {
        Ok(offset) => context.set_register(10, offset as usize),
        Err(errno) => return errno_flow(context, errno),
    }
    SyscallFlow::Resume
}

/// `pread` (`a0=fd, a1=buf_ptr, a2=len, a3=offset`)。検証規約は`read`と
/// 同じで、fdの保持するoffsetは動かさない。`ReadComplete`経路を再利用する。
fn dispatch_pread<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let fd = context.register(10);
    if let Some(errno) = unseekable_fd(source, fd) {
        context.set_register(10, errno as usize);
        return SyscallFlow::Resume;
    }
    let len = context.register(12);
    if len > MAX_READ_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    if len == 0 {
        context.set_register(10, 0);
        return SyscallFlow::Resume;
    }
    let start = context.register(11) as u64;
    // storageのlazy mountを起こし得るsourceへ触れる前にEFAULTを確定する。
    if check_user_writable_range(space, memory, start, len).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    match source.pread_fd(fd, context.register(13) as u64, &mut read_scratch[..len]) {
        Ok(count) => SyscallFlow::ReadComplete {
            start,
            len: count.min(len),
        },
        Err(errno) => errno_flow(context, errno),
    }
}

/// `pwrite` (`a0=fd, a1=buf_ptr, a2=len, a3=offset`)。検証規約は`write`
/// と同じで、fdの保持するoffsetは動かさない。
fn dispatch_pwrite<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let fd = context.register(10);
    if let Some(errno) = unseekable_fd(source, fd) {
        context.set_register(10, errno as usize);
        return SyscallFlow::Resume;
    }
    let len = context.register(12);
    if len > MAX_WRITE_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    if len == 0 {
        context.set_register(10, 0);
        return SyscallFlow::Resume;
    }
    let mut buffer = [0u8; MAX_WRITE_LEN];
    // storageのlazy mountを起こし得るsourceへ触れる前にEFAULTを確定する。
    if copy_from_user(
        space,
        memory,
        context.register(11) as u64,
        &mut buffer[..len],
    )
    .is_err()
    {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    match source.pwrite_fd(fd, context.register(13) as u64, &buffer[..len]) {
        Ok(count) => context.set_register(10, count.min(len)),
        Err(errno) => return errno_flow(context, errno),
    }
    SyscallFlow::Resume
}

/// `pipe` (`a0=out`)。`out`が指すwritableな8 byteへ`[read_fd, write_fd]`
/// をLEで書き込む。fd割り当てのside effectより先に`EFAULT`を確定する。
/// 成功時は`ReadComplete`経由でcopy-outされ、`a0`へ`PIPE_OUT_LEN`が返る。
fn dispatch_pipe<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let out = context.register(10) as u64;
    if check_user_writable_range(space, memory, out, PIPE_OUT_LEN).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    match source.create_pipe() {
        Ok((read_fd, write_fd)) => {
            read_scratch[..4].copy_from_slice(&(read_fd as u32).to_le_bytes());
            read_scratch[4..8].copy_from_slice(&(write_fd as u32).to_le_bytes());
            SyscallFlow::ReadComplete {
                start: out,
                len: PIPE_OUT_LEN,
            }
        }
        Err(errno) => errno_flow(context, errno),
    }
}

/// `close` (`a0=fd`)。fd 0/1/2も含めてsourceへ委譲し、slotを空ける。
/// 範囲外と未割当は`EBADF`。成功時は`a0`へ0を返す。
fn dispatch_close<E, R: ControlSource>(
    context: &mut UserContext,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let fd = context.register(10);
    if fd >= FD_TABLE_LEN {
        context.set_register(10, EBADF as usize);
        return SyscallFlow::Resume;
    }
    match source.close_fd(fd) {
        Ok(()) => context.set_register(10, 0),
        Err(errno) => return errno_flow(context, errno),
    }
    SyscallFlow::Resume
}

/// `dup2` (`a0=oldfd, a1=newfd`)。範囲外の`newfd`は`EBADF`、
/// `oldfd == newfd`は開いていれば変更せず`newfd`を返す。それ以外は
/// sourceが`newfd`を閉じてから`oldfd`のentryを複製する。
fn dispatch_dup2<E, R: ControlSource>(
    context: &mut UserContext,
    source: &mut R,
) -> SyscallFlow<E, R::Error> {
    let oldfd = context.register(10);
    let newfd = context.register(11);
    let result = if oldfd >= FD_TABLE_LEN || newfd >= FD_TABLE_LEN {
        Err(EBADF)
    } else {
        source.dup2(oldfd, newfd)
    };
    context.set_register(10, result.unwrap_or_else(|errno| errno as usize));
    SyscallFlow::Resume
}

/// `ReadComplete`の受信済みbyteを`copy_to_user`でuser rangeへ移し、
/// `a0`へ長さを書く。
///
/// 呼び出し側は`dispatch_read`へ渡したscratchの先頭`len` byteと、返却された
/// `start`と`len`をそのまま渡さなければならない。copyは`copy_to_user`が
/// `check_user_writable_range`と同じpage walkでU/Wを再検査しながら行う。
/// 対象processが停止しpage tableも不変なtrap窓内であれば失敗は起き得ないが、
/// 万一の場合は`a0`へ`EFAULT`を書いて留める。
pub fn complete_read<M: FrameStore>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &mut M,
    start: u64,
    len: usize,
    data: &[u8],
) {
    match copy_to_user(space, memory, start, &data[..len]) {
        Ok(()) => context.set_register(10, len),
        Err(_) => context.set_register(10, EFAULT as usize),
    }
}

/// `read_file` (`a0=path_ptr, a1=path_len, a2=buf_ptr, a3=buf_len`)。
/// mountでframeを確保し得るside effectの前に、path copyとbuf検証を
/// 済ませて`EFAULT`を確定する（`dispatch_read`と同じ規約）。成功時は
/// `ReadComplete`経由でscratchの先頭`n` byteがbufへcopyされる。
fn dispatch_read_file<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let path_len = context.register(11);
    let buf_len = context.register(13);
    if path_len == 0 || path_len > MAX_PATH_LEN || buf_len > MAX_READ_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    if buf_len == 0 {
        context.set_register(10, 0);
        return SyscallFlow::Resume;
    }

    let mut path_buf = [0u8; MAX_PATH_LEN];
    if copy_from_user(
        space,
        memory,
        context.register(10) as u64,
        &mut path_buf[..path_len],
    )
    .is_err()
    {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    let buf_start = context.register(12) as u64;
    if check_user_writable_range(space, memory, buf_start, buf_len).is_err() {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    };

    match source.read_file(path, &mut read_scratch[..buf_len]) {
        // sourceの契約は`output.len()`以下のcountだが、逸脱しても
        // scratchの範囲外をReadCompleteへ渡さないよう打ち切る。
        Ok(count) => SyscallFlow::ReadComplete {
            start: buf_start,
            len: count.min(buf_len),
        },
        Err(errno) => errno_flow(context, errno),
    }
}

/// `write` (`a0=fd, a1=ptr, a2=len`)。console出力のentryはframe sinkへ、
/// fileとpipeのentryは`source.write_fd`へ委譲する。どちらの経路でも、副作用の
/// 前にuser buffer全体の`EFAULT`を確定する規約は同じである。
fn dispatch_write<M: FrameStore, S: ControlSink, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    sink: &mut S,
    source: &mut R,
) -> SyscallFlow<S::Error, R::Error> {
    let fd = context.register(10);
    let kind = match source.console_fd(fd) {
        Some(ConsoleFd::Stdout) => Some(FrameKind::Stdout),
        Some(ConsoleFd::Stderr) => Some(FrameKind::Stderr),
        None if fd < FD_TABLE_LEN => None,
        _ => {
            context.set_register(10, EBADF as usize);
            return SyscallFlow::Resume;
        }
    };
    let len = context.register(12);
    if len > MAX_WRITE_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }

    // kernel stackに置く一次bufferは最大4,096 byteとする規約である。
    // 検証付きcopyが拒絶した場合、kernel panicではなくEFAULTとして返す。
    let mut buffer = [0u8; MAX_WRITE_LEN];
    match copy_from_user(
        space,
        memory,
        context.register(11) as u64,
        &mut buffer[..len],
    ) {
        Ok(()) => {}
        Err(_) => {
            context.set_register(10, EFAULT as usize);
            return SyscallFlow::Resume;
        }
    }

    match kind {
        Some(kind) => match sink.frame(kind, &buffer[..len]) {
            Ok(()) => {
                context.set_register(10, len);
                SyscallFlow::Resume
            }
            Err(error) => SyscallFlow::Fatal(error),
        },
        None => match source.write_fd(fd, &buffer[..len]) {
            Ok(count) => {
                context.set_register(10, count.min(len));
                SyscallFlow::Resume
            }
            // `read`と同じく、pipe満杯やI/O完了待ちでmark済みのcallerは
            // ecallへ戻して再実行する。
            Err(errno) => errno_flow(context, errno),
        },
    }
}

/// `sendto` (`a0=sock, a1=buf_ptr, a2=buf_len, a3=addr_ptr`)。payloadと
/// `SOCKADDR_LEN` byteの`SockAddr`を検証付きcopyで受け取り、
/// `source.send_to`へ委譲する。payloadは`read_scratch`を使うため
/// `MAX_READ_LEN`以下でなければならない（実際のdatagram上限は
/// `MAX_DATAGRAM_LEN`でstack側が検査する）。
fn dispatch_send_to<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let sock = context.register(10);
    let buf_len = context.register(12);
    if buf_len > MAX_READ_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    let mut addr = [0u8; SOCKADDR_LEN];
    if copy_from_user(space, memory, context.register(13) as u64, &mut addr).is_err()
        || copy_from_user(
            space,
            memory,
            context.register(11) as u64,
            &mut read_scratch[..buf_len],
        )
        .is_err()
    {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    // `ip`はdotted-quad順を整数で保持する（`10.0.2.15`なら`0x0A00020F`）。
    let dst_ip = u32::from_le_bytes([addr[0], addr[1], addr[2], addr[3]]).to_be_bytes();
    let dst_port = u16::from_le_bytes([addr[4], addr[5]]);
    match source.send_to(sock, &read_scratch[..buf_len], dst_ip, dst_port) {
        Ok(sent) => {
            context.set_register(10, sent);
            SyscallFlow::Resume
        }
        Err(errno) => errno_flow(context, errno),
    }
}

/// `recvfrom` (`a0=sock, a1=buf_ptr, a2=buf_len, a3=addr_ptr`)。bufと
/// `addr`の書き込み可能を先に確定させてから`source.recv_from`を呼び、
/// 受信があればpayloadをscratch先頭`len` byteへ、`SockAddr`を続く
/// `SOCKADDR_LEN` byteへ配置して`RecvComplete`を返す。未受信
/// (`Ok(None)`) は`EAGAIN`経由で`Blocked`となり、datagram到着後に
/// 同じecallがやり直される。
fn dispatch_recv_from<M: FrameStore, E, R: ControlSource>(
    context: &mut UserContext,
    space: &AddressSpace,
    memory: &M,
    source: &mut R,
    read_scratch: &mut [u8; MAX_READ_LEN],
) -> SyscallFlow<E, R::Error> {
    let sock = context.register(10);
    let buf_len = context.register(12);
    // payloadの後ろへsockaddrを置くため、buf_len + SOCKADDR_LENが
    // scratch内に収まることが条件である。
    if buf_len > MAX_READ_LEN - SOCKADDR_LEN {
        context.set_register(10, EINVAL as usize);
        return SyscallFlow::Resume;
    }
    let buf_start = context.register(11) as u64;
    let addr_start = context.register(13) as u64;
    if check_user_writable_range(space, memory, buf_start, buf_len).is_err()
        || check_user_writable_range(space, memory, addr_start, SOCKADDR_LEN).is_err()
    {
        context.set_register(10, EFAULT as usize);
        return SyscallFlow::Resume;
    }
    match source.recv_from(sock, &mut read_scratch[..buf_len]) {
        Ok(Some((src_ip, src_port, len))) => {
            // sourceの契約は`output.len()`以下だが、逸脱しても検証済みの
            // 範囲外を`RecvComplete`へ渡さないよう打ち切る。
            let len = len.min(buf_len);
            let mut addr = [0u8; SOCKADDR_LEN];
            addr[0..4].copy_from_slice(&u32::from_be_bytes(src_ip).to_le_bytes());
            addr[4..6].copy_from_slice(&src_port.to_le_bytes());
            // handler側は`scratch[len..len + SOCKADDR_LEN]`を`addr_start`へ
            // copyするため、sockaddrはpayloadの直後へ置く。
            read_scratch[len..len + SOCKADDR_LEN].copy_from_slice(&addr);
            SyscallFlow::RecvComplete {
                start: buf_start,
                len,
                addr_start,
            }
        }
        Ok(None) => errno_flow(context, EAGAIN),
        Err(errno) => errno_flow(context, errno),
    }
}
