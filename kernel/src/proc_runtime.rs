use super::*;

#[cfg(target_arch = "riscv64")]
/// guestの`read_file`が使うstorage session。shell sessionとは別にprobeし、
/// 確立に使ったqueue frameをこのstaticが存続させる。
static mut FILE_STORAGE: Option<shell::Rv64Storage> = None;

/// run終了時に`read_file`のstorage sessionを閉じ、queue frameをpoolへ返す。
///
/// # Safety
///
/// 全user processが終了し、trap handlerからの借用が残っていない後にだけ呼ぶこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn close_file_storage() {
    unsafe { *&raw mut FILE_STORAGE = None };
}

/// `read_file` syscall向けのstorage sessionを借りる。初回だけFDT probeと
/// FAT32 mountを行い、以降は同じsessionを返す。
///
/// # Safety
///
/// stdin stagingと同じくtrap handlerの実行窓からのみ呼び、借用をtrapの外へ
/// 持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn borrow_file_storage()
-> Result<&'static mut shell::Rv64Storage, shell::Rv64StorageError> {
    let storage = unsafe { &mut *&raw mut FILE_STORAGE };
    if storage.is_none() {
        *storage = Some(shell::probe_and_mount(&mut GlobalFrames)?);
    }
    Ok(storage.as_mut().expect("mounted above"))
}

/// 現在trap中のprocess。fd tableは`Process`が所有するため、handlerは
/// このpointer経由でだけ現在processのfdへ触れる。run loopが
/// `__run_user`の直前に設定し、復帰後にnullへ戻す。
#[cfg(target_arch = "riscv64")]
static mut CURRENT_PROC: *mut Process = core::ptr::null_mut();

/// 実行中のprocess table。`unlink`のfd失効は全processのfd tableを走査
/// する必要があるため、handlerはこのpointer経由でだけtableへ触れる。
/// run loopがdispatch開始前に設定し、終了後にnullへ戻す。
#[cfg(target_arch = "riscv64")]
static mut PROC_TABLE_PTR: *mut ProcessTable = core::ptr::null_mut();

/// `spawn`が新processへ複写するkernel mappingの計画。run loopが
/// dispatch開始前に設定し、終了後にnullへ戻す。`KernelMapPlan`は
/// kernel_mainのstack上でloopより長生きする。
#[cfg(target_arch = "riscv64")]
static mut KERNEL_MAP_PLAN_PTR: *const minios_kernel::vm::kernel::KernelMapPlan = core::ptr::null();

/// run loopが`__run_user`の直前と復帰後に呼び、fd opの対象processを指す。
/// `null`は「trap中のprocessなし」を意味する。
///
/// # Safety
///
/// `USER_SYSCALL_PROBE_*`と同じ契約: 実行窓のhandlerだけが読む値であり、
/// kernelへ戻るたびにnullへ戻すこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn set_current_process(process: *mut Process) {
    unsafe { *&raw mut CURRENT_PROC = process };
}

/// run loopがdispatch loopの開始前と終了後に呼び、fd失効の走査対象を指す。
///
/// # Safety
///
/// `set_current_process`と同じ契約: 実行窓のhandlerだけが読む値であり、
/// loop終了時にnullへ戻すこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn set_process_table(table: *mut ProcessTable) {
    unsafe { *&raw mut PROC_TABLE_PTR = table };
}

/// 現在processへfile記述子を割り当て、fd番号を返す。`writable`のfdは
/// `write`だけを受理し、それ以外のfdは`read`だけを受理する。
/// process未設定なら`ENOSYS`、空きslotがなければ`EMFILE`を返す。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼び、借用をtrapの外へ持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn alloc_file_fd(
    desc: minios_kernel::storage::fat32::FileDesc,
    writable: bool,
) -> Result<usize, isize> {
    let process = unsafe { *&raw const CURRENT_PROC };
    let Some(process) = (unsafe { process.as_mut() }) else {
        return Err(minios_abi::syscall::ENOSYS);
    };
    process.alloc_fd(desc, writable)
}

/// 現在processの`fd`に対応するentryを借りる。未割り当てやprocess未設定なら`None`。
/// file fdとpipe端の両方を返すため、file専用の操作は`FdEntry::file_mut`で
/// 絞り込むこと。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼び、借用をtrapの外へ持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn fd_entry_mut(
    fd: usize,
) -> Option<&'static mut minios_kernel::process::FdEntry> {
    let process = unsafe { *&raw const CURRENT_PROC };
    unsafe { process.as_mut() }?.fd_mut(fd)
}

/// 現在processの`fd`を閉じる。未割り当てなら`false`を返す。
/// pipe端を閉じた場合は同pipeのwaiterを起こし、peer側のEOF/EPIPEを
/// 再判定させる。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼び、借用をtrapの外へ持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn close_file_fd(fd: usize) -> bool {
    let process = unsafe { *&raw const CURRENT_PROC };
    let Some(process) = (unsafe { process.as_mut() }) else {
        return false;
    };
    let pipe_id = process
        .fd(fd)
        .and_then(minios_kernel::process::FdEntry::pipe_id);
    if !process.close_fd(fd) {
        return false;
    }
    if let Some(id) = pipe_id {
        let table = unsafe { *&raw const PROC_TABLE_PTR };
        if let Some(table) = unsafe { table.as_mut() } {
            table.wake_pipe_waiters(id);
        }
    }
    true
}

/// 現在processの`oldfd`を`newfd`へ複製し`newfd`を返す (`dup2` syscall)。
/// `newfd`が閉じたpipe端だった場合は`close`と同じく同pipeのwaiterを
/// 起こし、peer側のEOF/EPIPEを再判定させる。process未設定なら`ENOSYS`。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼び、借用をtrapの外へ持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn dup2_fd(oldfd: usize, newfd: usize) -> Result<usize, isize> {
    let process = unsafe { *&raw const CURRENT_PROC };
    let Some(process) = (unsafe { process.as_mut() }) else {
        return Err(minios_abi::syscall::ENOSYS);
    };
    let replaced = process.dup2(oldfd, newfd)?;
    if let Some(id) = replaced
        .as_ref()
        .and_then(minios_kernel::process::FdEntry::pipe_id)
    {
        let table = unsafe { *&raw const PROC_TABLE_PTR };
        if let Some(table) = unsafe { table.as_mut() } {
            table.wake_pipe_waiters(id);
        }
    }
    Ok(newfd)
}

/// `pipe` syscallのfd割り当て。pipeをtableからallocし、現在processへ
/// read/write両端を割り当てる。write端の割り当てに失敗した場合は
/// read端をrollbackして`EMFILE`を返す。tableやprocessが未設定なら
/// `ENOSYS`、pipe slot枯渇は`ENOMEM`。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼び、借用をtrapの外へ持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn create_pipe() -> Result<(usize, usize), isize> {
    use minios_abi::syscall::{ENOMEM, ENOSYS};

    let table = unsafe { *&raw const PROC_TABLE_PTR };
    let Some(table) = (unsafe { table.as_mut() }) else {
        return Err(ENOSYS);
    };
    let process = unsafe { *&raw const CURRENT_PROC };
    let Some(process) = (unsafe { process.as_mut() }) else {
        return Err(ENOSYS);
    };
    let Some(id) = table.pipe_alloc() else {
        return Err(ENOMEM);
    };
    let read_fd = process.alloc_pipe_fd(id, false)?;
    match process.alloc_pipe_fd(id, true) {
        Ok(write_fd) => Ok((read_fd, write_fd)),
        Err(error) => {
            // 片端だけを残すと「write端0の空pipe」へreadが永遠に
            // blockし得るため、read端を巻き戻す。死んだslotは次の
            // `pipe_alloc`が再利用する。
            process.close_fd(read_fd);
            Err(error)
        }
    }
}

/// `read`がpipe read端へ委譲するtable側処理。`Ok(n)`は読んだbyte数、
/// `Ok(0)`は全write端のcloseによるEOF、`Err(EAGAIN)`はcallerを
/// `BlockedOnPipe`へmark済みのblock要求（dispatchがecallへ巻き戻す）。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼び、借用をtrapの外へ持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn pipe_read(id: usize, output: &mut [u8]) -> Result<usize, isize> {
    use minios_abi::syscall::{EAGAIN, EBADF, ENOSYS};
    use minios_kernel::process::PipeReadOutcome;

    let Some(caller) = (unsafe { current_pid() }) else {
        return Err(ENOSYS);
    };
    let table = unsafe { *&raw const PROC_TABLE_PTR };
    let Some(table) = (unsafe { table.as_mut() }) else {
        return Err(ENOSYS);
    };
    match table.pipe_read(caller, id, output) {
        Some(PipeReadOutcome::Read(count)) => Ok(count),
        Some(PipeReadOutcome::Blocked) => Err(EAGAIN),
        Some(PipeReadOutcome::Eof) => Ok(0),
        None => Err(EBADF),
    }
}

/// `write`がpipe write端へ委譲するtable側処理。`Ok(n)`は書いたbyte数、
/// `Err(EAGAIN)`はcallerを`BlockedOnPipe`へmark済みのblock要求、
/// `Err(EPIPE)`はread端の全滅。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼び、借用をtrapの外へ持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn pipe_write(id: usize, data: &[u8]) -> Result<usize, isize> {
    use minios_abi::syscall::{EAGAIN, EBADF, ENOSYS, EPIPE};
    use minios_kernel::process::PipeWriteOutcome;

    let Some(caller) = (unsafe { current_pid() }) else {
        return Err(ENOSYS);
    };
    let table = unsafe { *&raw const PROC_TABLE_PTR };
    let Some(table) = (unsafe { table.as_mut() }) else {
        return Err(ENOSYS);
    };
    match table.pipe_write(caller, id, data) {
        Some(PipeWriteOutcome::Wrote(count)) => Ok(count),
        Some(PipeWriteOutcome::Blocked) => Err(EAGAIN),
        Some(PipeWriteOutcome::Broken) => Err(EPIPE),
        None => Err(EBADF),
    }
}

/// `(dir_cluster, dir_index)`のdir entryを指すfdを全processのtableから
/// 閉じる。`unlink`したfileのclusterは即座に解放されるため、開いたままの
/// fdを残すと再利用されたslotやclusterを壊し得る。呼び出しprocess自身の
/// fdも失効する。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼び、借用をtrapの外へ持ち出さないこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn revoke_file_fds(dir_cluster: u32, dir_index: u32) {
    let table = unsafe { *&raw const PROC_TABLE_PTR };
    if let Some(table) = unsafe { table.as_mut() } {
        table.revoke_file_fds(dir_cluster, dir_index);
    }
}

/// `(old_cluster, old_index)`のdir entryを指すfdを全processで
/// `(new_cluster, new_index)`へ追従させる。cross-directory moveで
/// entryの物理位置が変わってもopen file fdを有効に保つ。
/// `PROC_TABLE_PTR`はtrap handlerの実行窓内でのみderef可能。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn relocate_file_fds(
    old_cluster: u32,
    old_index: u32,
    new_cluster: u32,
    new_index: u32,
) {
    let table = unsafe { *&raw const PROC_TABLE_PTR };
    if let Some(table) = unsafe { table.as_mut() } {
        table.relocate_file_fds(old_cluster, old_index, new_cluster, new_index);
    }
}

/// run loopがdispatch loopの開始前と終了後に呼び、`spawn`が複写する
/// kernel mapping計画を指す。
///
/// # Safety
///
/// `set_process_table`と同じ契約: 実行窓のhandlerだけが読む値であり、
/// loop終了時にnullへ戻すこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn set_kernel_map_plan(plan: *const minios_kernel::vm::kernel::KernelMapPlan) {
    unsafe { *&raw mut KERNEL_MAP_PLAN_PTR = plan };
}

/// 現在processのpidを返す。trap中のprocessがなければ`None`。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn current_pid() -> Option<usize> {
    let process = unsafe { *&raw const CURRENT_PROC };
    unsafe { process.as_ref() }.map(Process::pid)
}

/// file I/Oの完了待ちでcallerを`BlockedOnIo`へ移す。storage errno写像が
/// `VirtioError::WouldBlock`を`EAGAIN`へ変える直前に呼び、dispatchの
/// `Blocked`経路で同じecallが後でやり直されることを保証する。
/// process contextがなければ何もしない（kernel shell経路では発生しない）。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn block_current_on_io() {
    let process = unsafe { *&raw const CURRENT_PROC };
    if let Some(process) = unsafe { process.as_mut() } {
        process.block_on_io();
    }
}

/// ELFを新processとして読み込む。kernel mappingは`KERNEL_MAP_PLAN_PTR`、
/// user memoryは`USER_SYSCALL_PROBE_MEMORY`、frameは`GlobalFrames`を使う。
/// `USER_SYSCALL_PROBE_MEMORY`を指す`&`参照はこの呼び出し中に再使用
/// されないため、実行窓の排他契約で`&mut`へ変換できる。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。返したprocessの所有権はcallerへ
/// 移り、tableへのinsertかreclaimをcallerが行う。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn spawn_process(
    name: &'static str,
    elf: &[u8],
    arguments: &[&str],
) -> Result<
    Process,
    minios_kernel::process::SpawnFailure<minios_kernel::vm::storage::IdentityFrameStoreError>,
> {
    use minios_kernel::process::SpawnError;

    let plan = unsafe { *&raw const KERNEL_MAP_PLAN_PTR };
    let Some(plan) = (unsafe { plan.as_ref() }) else {
        return Err(minios_kernel::process::SpawnFailure {
            error: SpawnError::OutOfFrames,
            image: None,
        });
    };
    let memory = unsafe { &mut *(USER_SYSCALL_PROBE_MEMORY as *mut IdentityFrameStore) };
    let mut frames = GlobalFrames;
    // callerのfd tableをsnapshotで引き継ぐ。`CURRENT_PROC`はsyscall経路では
    // 必ず設定済みだが、未設定でも空tableでspawnできる形にする。
    let caller = unsafe { *&raw const CURRENT_PROC };
    let file_fds = unsafe { caller.as_ref() }
        .map_or_else(minios_kernel::process::FileFdTable::new, |process| {
            process.file_fds_snapshot()
        });
    Process::spawn(
        name,
        elf,
        arguments,
        file_fds,
        &mut frames,
        memory,
        plan.mappings(),
    )
}

/// `spawn`で作ったprocessをtableへ登録し、採番したpidを返す。
/// table満杯時はprocessをそのまま返すため、callerが所有frameを回収する。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
#[allow(clippy::result_large_err)]
pub(crate) unsafe fn insert_spawned(process: Process) -> Result<usize, Process> {
    let table = unsafe { *&raw const PROC_TABLE_PTR };
    match unsafe { table.as_mut() } {
        Some(table) => table.insert(process),
        None => Err(process),
    }
}

/// `waitpid`のtable側処理。`target`の終了codeが台帳にあれば消費して
/// `Ok(Some(code))`、liveならcallerを`BlockedOnPid(target)`へ移して
/// `Ok(None)`（callerは同じecallを再実行してcodeを回収する）、
/// self/不在/既reapは`ECHILD`、wait連鎖のcycleは`EINVAL`。
/// process contextがなければ`ENOSYS`。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。callerの`BlockedOnPid` markは
/// table経由で現在processへ書くため、run loopがBlockedを受けて呼ぶ
/// `block_on_stdin`（runnable時のみ遷移）で上書きされない。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn wait_pid(target: usize) -> Result<Option<u32>, isize> {
    let Some(caller) = (unsafe { current_pid() }) else {
        return Err(minios_abi::syscall::ENOSYS);
    };
    let table = unsafe { *&raw const PROC_TABLE_PTR };
    let Some(table) = (unsafe { table.as_mut() }) else {
        return Err(minios_abi::syscall::ENOSYS);
    };
    if let Some(code) = table.take_exit(target) {
        return Ok(Some(code));
    }
    table.block_waitpid(caller, target).map(|()| None)
}

/// `kill`のtable側処理。`target`を`Killed`へmarkして`Ok(())`を返し、
/// 実際の回収と`waitpid`への終了code通知はrun loopが行う。`ECHILD`は
/// 対象不在、`EINVAL`は自分自身、process contextがなければ`ENOSYS`。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。`CURRENT_PROC`やdispatch中の
/// 他processへのpointerが`Vec::remove`でずれないよう、ここではmark
/// だけを行い`take`しない。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn kill_pid(target: usize) -> Result<(), isize> {
    let Some(caller) = (unsafe { current_pid() }) else {
        return Err(minios_abi::syscall::ENOSYS);
    };
    let table = unsafe { *&raw const PROC_TABLE_PTR };
    let Some(table) = (unsafe { table.as_mut() }) else {
        return Err(minios_abi::syscall::ENOSYS);
    };
    table.mark_killed(caller, target)
}

/// 現在processのimageを`elf`で置き替える (`exec`)。kernel stackとfd
/// tableは引き継ぎ、成功時は旧imageをprocessの`retired_image`へ退避して
/// 新imageの初期contextを返す。旧imageの解放はkernel satpへ戻った後の
/// run loopが行う。失敗時はprocessに触れずcallerは旧imageで動き続ける。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。`spawn_process`と同じく
/// `KERNEL_MAP_PLAN_PTR`と`USER_SYSCALL_PROBE_MEMORY`を使う。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn exec_current_process(
    name: &'static str,
    elf: &[u8],
) -> Result<
    UserContext,
    minios_kernel::process::SpawnFailure<minios_kernel::vm::storage::IdentityFrameStoreError>,
> {
    use minios_kernel::process::SpawnError;

    let process = unsafe { *&raw const CURRENT_PROC };
    let Some(process) = (unsafe { process.as_mut() }) else {
        return Err(minios_kernel::process::SpawnFailure {
            error: SpawnError::OutOfFrames,
            image: None,
        });
    };
    let plan = unsafe { *&raw const KERNEL_MAP_PLAN_PTR };
    let Some(plan) = (unsafe { plan.as_ref() }) else {
        return Err(minios_kernel::process::SpawnFailure {
            error: SpawnError::OutOfFrames,
            image: None,
        });
    };
    let memory = unsafe { &mut *(USER_SYSCALL_PROBE_MEMORY as *mut IdentityFrameStore) };
    let mut frames = GlobalFrames;
    process.exec(name, elf, &[], &mut frames, memory, plan.mappings())
}

/// 現在processのbreakを動かす (`sbrk` syscall)。frameは`GlobalFrames`、
/// page tableへの書き込みは`USER_SYSCALL_PROBE_MEMORY`を使う。
/// 新しいPTEはactiveなuser satpへ入るため、戻る前にTLBを無効化する。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn sbrk_current_process(increment: isize) -> Result<u64, isize> {
    let process = unsafe { *&raw const CURRENT_PROC };
    let Some(process) = (unsafe { process.as_mut() }) else {
        return Err(minios_abi::syscall::ENOSYS);
    };
    let memory = unsafe { &mut *(USER_SYSCALL_PROBE_MEMORY as *mut IdentityFrameStore) };
    let mut frames = GlobalFrames;
    let result = process.sbrk(increment, &mut frames, memory);
    // Safety: S-modeのtrap窓で、変更したpage tableの古い翻訳を捨てるだけである。
    unsafe { core::arch::asm!("sfence.vma zero, zero") };
    result
}

/// 現在processを`millis`後の起床tickまでsleep状態へmarkする (`sleep`
/// syscall)。dispatchは続けて`Yield`を返し、run loopは`wake_sleepers`が
/// 起こすまでこのprocessを選ばない。process contextがなければ`ENOSYS`。
///
/// # Safety
///
/// trap handlerの実行窓からのみ呼ぶこと。
#[cfg(target_arch = "riscv64")]
#[allow(clippy::deref_addrof)]
pub(crate) unsafe fn sleep_current_process(millis: usize) -> Result<(), isize> {
    let process = unsafe { *&raw const CURRENT_PROC };
    let Some(process) = (unsafe { process.as_mut() }) else {
        return Err(minios_abi::syscall::ENOSYS);
    };
    process.block_until(time::sleep_deadline(time::ticks(), millis));
    Ok(())
}
