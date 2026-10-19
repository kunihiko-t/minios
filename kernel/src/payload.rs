use super::*;
use crate::proc_runtime::*;
use crate::user_trap::*;

/// 終了したprocessをtableから取り出して全所有frameを回収する。
/// fd tableはprocess内に閉じるため`take`とともに死ぬ。socket tableは
/// process外の共有resourceなので、所有者をpidで引いてここで解放する。
/// 回収に失敗した場合は一回だけ再試行し、それでも駄目ならpayload全体を中止する。
#[cfg(target_arch = "riscv64")]
fn reclaim_process_slot(table: &mut ProcessTable, pid: usize, frames: &mut dyn FrameSource) {
    let Some(mut process) = table.take(pid) else {
        return;
    };
    // Safety: 実行窓の外側であり、trap handlerからの借用は残っていない。
    unsafe { close_net_sockets_for(pid) };
    if process.reclaim(frames).is_err() && process.reclaim(frames).is_err() {
        fatal_payload_error(format_args!("MiniOS payload: reclaim pid={pid} failed\r\n"));
    }
}

/// statusを台帳へ記録して終了を通知し、waiterの起床後に所有resourceを回収する。
/// statusなしは単一imageのfatalやkernel側障害であり、成功のExitを送らない。
#[cfg(target_arch = "riscv64")]
fn complete_process(
    table: &mut ProcessTable,
    pid: usize,
    code: Option<u32>,
    multi: bool,
    sink: &mut control::UartControlSink,
    frames: &mut dyn FrameSource,
) {
    if let Some(code) = code {
        table.record_exit(pid, code);
        use minios_kernel::user::syscall::ControlSink as _;
        if multi {
            let payload = minios_abi::control::ProcExitPayload {
                pid: pid as u32,
                code,
            }
            .encode();
            let _ = sink.frame(minios_abi::control::FrameKind::ProcExit, &payload);
        } else {
            let _ = sink.frame(minios_abi::control::FrameKind::Exit, &code.to_le_bytes());
        }
    }
    table.wake_on_exit(pid);
    reclaim_process_slot(table, pid, frames);
}

/// tableに残る全processを回収する。spawn途中の失敗経路で使う。
#[cfg(target_arch = "riscv64")]
fn reclaim_process_table(table: &mut ProcessTable, frames: &mut dyn FrameSource) {
    while let Some(mut process) = table.take_oldest() {
        let _ = process.reclaim(frames);
    }
}

/// production payload path: Ready frameを送り、予約窓の全imageをprocessとして
/// spawnし、timerプリエンプションのround-robinで全processが終了するまで回す。
///
/// `read`は入力未到着のとき`Blocked`としてkernelへ戻り、processはstdin待ち
/// 状態で再選対象から外れる。入力が届くとecallがやり直されて完了する。
/// Stdin frameの受信は`StdinStaging`の再開可能なdecoderがbyte単位で蓄積し、
/// frame途中でbyteが尽きても`WouldBlock`として再び`Blocked`へ戻るため、
/// 受信途中の間も他processが進み続ける。
#[cfg(target_arch = "riscv64")]
pub(crate) fn run_boot_payload(
    kernel_space: &AddressSpace,
    plan: &KernelMapPlan,
    frames: &mut dyn FrameSource,
    memory: &mut IdentityFrameStore,
    payload: BootPayload<'static>,
) -> ! {
    let before = frames.stats();
    let kernel_root = PhysPageNum::from_start(kernel_space.root().as_u64())
        .expect("kernel root page number is valid");
    let kernel_satp = arch::riscv64::csr::sv39_satp_bits(kernel_root);
    let image_count = payload.images().count();
    // manifest v2ではProcExit frameでprocess個別の終了を通知し、v1互換の
    // 単一imageでは従来のExit frameを送る。
    let multi = payload.manifest().version() >= 2;

    // image宣言順にspawnし、slot index (=pid) をmanifest順と一致させる。
    // 所有権台帳は各processが内部所有する。静的arenaは要らない。
    let mut table = ProcessTable::new();
    for (index, spec) in payload.images().enumerate() {
        let mut argv: [&str; minios_abi::manifest::ARG_MAX_COUNT] =
            [""; minios_abi::manifest::ARG_MAX_COUNT];
        let mut argc = 0usize;
        for argument in spec.args() {
            argv[argc] = argument;
            argc += 1;
        }
        match Process::spawn(
            spec.name(),
            payload.image_elf(&spec),
            &argv[..argc],
            minios_kernel::process::FileFdTable::new(),
            frames,
            memory,
            plan.mappings(),
        ) {
            Ok(process) => {
                let pid = table
                    .insert(process)
                    .expect("manifest caps image count at MAX_PROCS");
                if pid != index {
                    fatal_payload_error(format_args!(
                        "MiniOS payload: unexpected pid {pid} for image {index}\r\n"
                    ));
                }
            }
            Err(failure) => {
                // 回収しきれなかったimageが残っていればdestroyを一度試す。
                if let Some(image) = failure.image {
                    let _ = image.destroy(frames);
                }
                reclaim_process_table(&mut table, frames);
                fatal_payload_error(format_args!(
                    "MiniOS payload: spawn {index}, {:?}\r\n",
                    failure.error
                ));
            }
        }
    }

    // user trap入口を指し直し、Supervisor timer割り込みを有効化する。
    // `time::init`が立てたsstatus.SIEはboot以降1のままなので、このまま
    // `stvec`をuser入口へ向けるとS-mode実行中に届いたtickがsscratch未設定の
    // `__user_trap_entry`へ飛び、context保存先がアドレス0付近へずれて壊れる。
    // U-mode中の割り込み配送はsstatus.SIEに依らないため、実行窓ではS-mode側を
    // 遮断してから`stvec`と`sie`を設定する。dispatch loop中のS-modeでは
    // trap時にSIEが落ちるため、この後に再び立つことはない。
    const SIE_STIE: usize = 1 << 5;
    const SIE_SEIE: usize = 1 << 9;
    // Safety: S-modeで`sstatus`、`stvec`、`sie`を書く。
    unsafe {
        let sstatus = arch::riscv64::csr::read_sstatus();
        arch::riscv64::csr::write_sstatus(sstatus & !SSTATUS_SIE);
        arch::riscv64::csr::write_stvec(__user_trap_entry as *const () as usize);
        // timer割り込みに加えてexternal割り込みもU-mode実行窓へ配送する。
        // 届いたIRQは`handle_user_trap`のExternal分岐がclaimする。
        let sie = arch::riscv64::csr::read_sie();
        arch::riscv64::csr::write_sie(sie | SIE_STIE | SIE_SEIE);
        USER_STDIN_STAGING = StdinStaging::new();
    }

    // boot中に積み上がったtick債務を精算する。S-mode側を遮断したためSTIPは
    // pendingのまま残り得て、そのままU-modeへ落ちると最初のprocessがほぼ
    // 進まずpreemptされる。ここでdeadlineを張り直し、最初のdispatchへ
    // 満タンのquantumを渡す。
    if let Err(error) = time::handle_interrupt() {
        fatal_payload_error(format_args!("MiniOS payload: timer, {error:?}\r\n"));
    }

    // network stackはここでprobeする。lazyにすると最初の`socket`呼び出しに
    // device initのコストが乗り、そのprocessのquantumを使い切って途中で
    // preemptされ得る。deviceが無ければ失敗を捨て、`socket`側のlazy probe
    // がそのまま再試行する。
    // Safety: dispatch前で借用は残っていない。
    #[cfg(target_arch = "riscv64")]
    unsafe {
        init_net_stack();
    }

    // Ready frameを最後のplain text出力の後に送り、以降のUARTをcontrol frame
    // へ限定する。
    control::send_ready();
    for process in table.iter() {
        crate::println!(
            "MiniOS sched: spawned pid={} name={}",
            process.pid(),
            process.name()
        );
    }

    // Safety: fd失効の走査と`spawn`のplan参照はdispatchのtrap窓からのみ
    // 行われ、loop終了時にnullへ戻す。
    unsafe {
        set_process_table(&mut table);
        set_kernel_map_plan(plan);
    }
    let mut sink = control::UartControlSink;
    let mut switches = 0usize;
    let mut failed = 0usize;
    let mut last_pid = usize::MAX;
    let mut last_code = 0u32;
    while !table.is_empty() {
        // stdinへbyteが届いていればblocked processを選対象へ戻す。
        // virtioのused-buffer割り込みも同様に、`BlockedOnIo`で寝ている
        // processを起こすedgeを消費する。
        if console::stdin_pending() || crate::drivers::plic::take_blk_io_notified() {
            table.wake_all_blocked();
        }
        // deadlineに達したsleep中processを選対象へ戻す。
        table.wake_sleepers(time::ticks());
        // `kill`がmarkしたprocessをdispatchの外で回収する。trap窓の内側で
        // `take`すると`Vec::remove`で他processへのpointerがずれるため、
        // killは`Killed`のmarkだけを残して終了code`EXIT_CODE_KILLED`の
        // 台帳記録・frame送信・waiter起床・resource回収をここへ委ねる。
        let killed = table.killed_pids().collect::<alloc::vec::Vec<usize>>();
        for killed_pid in killed {
            complete_process(
                &mut table,
                killed_pid,
                Some(minios_abi::syscall::EXIT_CODE_KILLED),
                multi,
                &mut sink,
                frames,
            );
        }
        let Some(pid) = table.pick_next() else {
            // stdin・device・timerのどれも起床源にならないなら、残りは
            // pid待ちかpipe待ちの閉路であり、`wfi`で待っても状態は変わらない。
            if table.deadlocked() {
                fatal_payload_error(format_args!(
                    "MiniOS payload: deadlock: processes={} blocked on pid or pipe with no wake source\r\n",
                    table.len()
                ));
            }
            // 占有slotが残るのに選べない＝全processがstdinかsleepなど、
            // 外部から起こせる待ちに入っている。
            // ここはS-mode割り込みを遮断したままなので、timer割り込みは
            // trapせず`sip.STIP`がpendingになるだけでtickは進まない。external
            // 割り込みも同様にSEIPへ積まれるため、claim/dispatchを明示的に呼ぶ。
            // stdinのdata-readyとSTIPをpollしつつ、間は`wfi`で眠る。`wfi`は
            // sstatus.SIEに依らずsieで許可されたpending割り込みで起床するため、
            // UART受信か次のtickで必ず戻る。loop先頭へ戻ると`wake_sleepers`が
            // 起床を判定する。
            loop {
                // 起床したirqがあればSEIP pendingを確実にclaimする。stdinの
                // byte到着を先に判定すると、届いたirqをclaimせずループを
                // 抜けてしまうため、判定より先に処理する。
                crate::drivers::plic::handle_external_pending();
                if console::stdin_pending()
                    || time::interrupt_pending()
                    || crate::drivers::plic::blk_io_notified()
                {
                    break;
                }
                // Safety: STIE/SEIEを立てた`wfi`であり、timerかexternalの
                // pending割り込みが起床させる。PLIC未初期化のmachineでは
                // timer割り込みだけが起床源となる。
                unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
            }
            if time::interrupt_pending()
                && let Err(error) = time::handle_interrupt()
            {
                fatal_payload_error(format_args!("MiniOS payload: timer, {error:?}\r\n"));
            }
            continue;
        };
        let process = table.get_mut(pid).expect("picked pid is live");
        // Safety: handlerがdispatch中だけ読む静的参照であり、kernelへ戻るたび
        // に0へ戻す。同時に実行されるprocessは1つだけなのでaliasしない。
        unsafe {
            USER_SYSCALL_PROBE_SPACE = process.address_space() as *const _ as usize;
            USER_SYSCALL_PROBE_MEMORY = memory as *const IdentityFrameStore as usize;
            USER_EXIT_CODE.store(usize::MAX, Ordering::Relaxed);
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_NONE, Ordering::Relaxed);
            set_current_process(process);
            // file I/Oがdevice完了待ちになったとき`WouldBlock`として返し、
            // callerを`BlockedOnIo`で寝かせられるようにする。戻ったら即解除。
            crate::storage::virtio_blk::set_io_block_allowed(true);
        }
        // Safety: processがuser address space・連続した専用kernel stack・保存済み
        // contextを所有する。assemblyはkernel satpとboot stackを復元して戻る。
        let exit = unsafe {
            __run_user(
                process.context_ptr(),
                process.user_satp(),
                kernel_satp,
                process.kernel_stack_top(),
            )
        };
        // Safety: `ReturnToKernel`の時点でこのprocessのtrap frame slotに
        // 中断contextが書き込まれている。
        unsafe {
            process.reload_context();
            USER_SYSCALL_PROBE_SPACE = 0;
            USER_SYSCALL_PROBE_MEMORY = 0;
            set_current_process(core::ptr::null_mut());
            crate::storage::virtio_blk::set_io_block_allowed(false);
        }
        if last_pid != usize::MAX && last_pid != pid {
            switches += 1;
        }
        last_pid = pid;
        match (exit, USER_RUN_OUTCOME.load(Ordering::Relaxed)) {
            (RunExit::ReturnToKernel, USER_RUN_OUTCOME_PREEMPTED) => {}
            (RunExit::ReturnToKernel, USER_RUN_OUTCOME_BLOCKED) => {
                process.block_on_stdin();
            }
            (RunExit::ReturnToKernel, USER_RUN_OUTCOME_EXEC) => {
                // kernel satpへ戻ったため旧imageのpage tableはinactive。
                // `reload_context`はtrap slotの新初期contextを拾い済みで、
                // processはrunnableのまま残り、次のdispatchで新imageを走る。
                // 解放失敗はreclaimと同じく一回だけ再試行する。
                if let Some(retired) = process.take_retired_image()
                    && let Err(error) = retired.destroy(frames)
                {
                    let (_, image) = error.into_parts();
                    if image.destroy(frames).is_err() {
                        fatal_payload_error(format_args!(
                            "MiniOS payload: exec reclaim pid={pid} failed\r\n"
                        ));
                    }
                }
            }
            (RunExit::ReturnToKernel, USER_RUN_OUTCOME_EXIT) => {
                let code = USER_EXIT_CODE.load(Ordering::Relaxed) as u32;
                last_code = code;
                complete_process(&mut table, pid, Some(code), multi, &mut sink, frames);
            }
            (RunExit::ReturnToKernel, USER_RUN_OUTCOME_FATAL_TRAP) => {
                let scause = USER_FATAL_SCAUSE.load(Ordering::Relaxed);
                let stval = USER_FATAL_STVAL.load(Ordering::Relaxed);
                if multi {
                    // 故障したタスクだけを終了し、待っているタスクへstatusを渡す。
                    crate::println!(
                        "MiniOS sched: fault pid={pid} code={USER_FAULT_EXIT_CODE} scause={scause:#018x} stval={stval:#018x}"
                    );
                    complete_process(
                        &mut table,
                        pid,
                        Some(USER_FAULT_EXIT_CODE),
                        multi,
                        &mut sink,
                        frames,
                    );
                } else {
                    // 既存の単一image契約ではGuestErrorと異常shutdownを維持する。
                    failed += 1;
                    crate::console::emergency_print(format_args!(
                        "MiniOS user trap: scause={scause:#018x} stval={stval:#018x}\r\n"
                    ));
                    complete_process(&mut table, pid, None, multi, &mut sink, frames);
                }
            }
            (
                RunExit::ReturnToKernel,
                USER_RUN_OUTCOME_SINK_FAILURE | USER_RUN_OUTCOME_SOURCE_FAILURE,
            ) => {
                failed += 1;
                complete_process(&mut table, pid, None, multi, &mut sink, frames);
            }
            // `ReturnToKernel`以外の戻りや、handlerがoutcomeを記録しないまま
            // 戻った場合は実装不変条件の破綻であり、静かに続行しない。
            _ => fatal_payload_error(format_args!(
                "MiniOS payload: internal, exit={exit:?} outcome={}\r\n",
                USER_RUN_OUTCOME.load(Ordering::Relaxed)
            )),
        }
    }

    // guestが開いたstorage sessionとnetwork stackを閉じ、queue/rx frameを
    // poolへ返す。全processが終了済みでhandlerからの借用は残っていない。
    unsafe {
        set_process_table(core::ptr::null_mut());
        set_kernel_map_plan(core::ptr::null());
        close_file_storage();
        close_net_stack();
    }

    #[cfg(feature = "qemu-test-irq-uart")]
    {
        // UART受信byteがPLIC claim経由で観測されたことのmarker。評価点を
        // 終了時に置くのは、入力がreaderのblock前にstagingへ届いたrunでは
        // blocked-loop自体に入らず評価がskipされるため。byte供給経路は全て
        // claim-firstなので、受信があればclaimは必ず観測される。
        if crate::drivers::plic::uart_irq_claims() > 0 {
            crate::println!("[MINIOS_TEST] irq-uart: ok");
        }
    }
    #[cfg(feature = "qemu-test-irq-blk")]
    {
        // disk I/Oの完了がpollingではなくPLIC claim経由だったことの観測
        // marker。used-buffer割り込みが一度もclaimされなければmarkerを出さず、
        // test側のtimeoutで検出する。
        if crate::drivers::plic::virtio_irq_claims() > 0 {
            crate::println!("[MINIOS_TEST] irq-blk: ok");
        }
    }
    let after = frames.stats();
    if after != before {
        fatal_payload_error(format_args!(
            "MiniOS payload: recovery, allocator expected={before:?} actual={after:?}\r\n"
        ));
    }
    if failed > 0 {
        fatal_payload_error(format_args!(
            "MiniOS payload: failed processes={failed}\r\n"
        ));
    }
    if multi {
        crate::println!("\r\nMiniOS payload: ok processes={image_count} switches={switches}");
    } else {
        crate::println!("\r\nMiniOS payload: ok code={last_code}");
    }
    successful_payload_shutdown()
}

#[cfg(target_arch = "riscv64")]
fn successful_payload_shutdown() -> ! {
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::NoReason,
    )
}
