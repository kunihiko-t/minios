use super::*;

#[cfg(target_arch = "riscv64")]
unsafe extern "C" {
    // Safety: `user.S`が`sret`でU-modeへ降りる唯一の入口として公開するC ABI境界である。
    pub(crate) fn __run_user(
        context: *mut UserContext,
        user_satp: u64,
        kernel_satp: u64,
        kernel_stack_top: usize,
    ) -> RunExit;
    // Safety: `user.S`が公開するuser trap入口であり、4バイト境界にそろったsymbolである。
    pub(crate) fn __user_trap_entry();
}

#[cfg(target_arch = "riscv64")]
// `user.S`がシンボル名とC ABIを直接指定して呼ぶため、この名前とABIを変えてはならない。
#[unsafe(no_mangle)]
extern "C" fn rust_user_trap_handler(context: *mut UserContext) -> RunExit {
    // Safety: `user.S`がtrap frameとして組み立てた直後のkernel stack上の
    // 有効なcontextだけを渡す。
    unsafe { rust_user_trap_handler_impl(context) }
}

/// user trap入口が受けたS-mode由来のtrap。syscall処理中のkernel stack溢れは、
/// guard pageへのstore/load page faultとしてここへ届く。`fault_sp`はtrap時の
/// sp、`stack_top`は直近の`__run_user`へ渡したkernel stack topである。
/// 元のstackは信用できないため、`user.S`の専用stack上で診断を出して停止する。
#[cfg(target_arch = "riscv64")]
// `user.S`がシンボル名とC ABIを直接指定して呼ぶため、この名前とABIを変えてはならない。
#[unsafe(no_mangle)]
extern "C" fn rust_kernel_trap_in_user_window(fault_sp: usize, stack_top: usize) -> ! {
    const LOAD_PAGE_FAULT: usize = 13;
    const STORE_PAGE_FAULT: usize = 15;
    let scause = arch::riscv64::csr::read_scause();
    let sepc = arch::riscv64::csr::read_sepc();
    let stval = arch::riscv64::csr::read_stval();
    if matches!(scause, LOAD_PAGE_FAULT | STORE_PAGE_FAULT)
        && minios_kernel::user::run::kernel_stack_guard_contains(stack_top, stval)
    {
        crate::console::emergency_print(format_args!(
            "\r\nMiniOS kernel stack overflow: sepc={sepc:#018x} stval={stval:#018x} sp={fault_sp:#018x} stack_top={stack_top:#018x}\r\n"
        ));
        finish_kernel_stack_overflow()
    }
    crate::console::emergency_print(format_args!(
        "\r\nMiniOS kernel trap: scause={scause:#018x} sepc={sepc:#018x} stval={stval:#018x} sp={fault_sp:#018x}\r\n"
    ));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(all(target_arch = "riscv64", not(feature = "qemu-test-kernel-stack")))]
fn finish_kernel_stack_overflow() -> ! {
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

/// kernel-stack probeでは、guard pageで止まったことが成功条件である。
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-kernel-stack"))]
fn finish_kernel_stack_overflow() -> ! {
    crate::console::emergency_print(format_args!(
        "[MINIOS_TEST] kernel-stack: overflow detected\r\n"
    ));
    successful_qemu_test_shutdown()
}

/// syscall処理中のtrap stackを、guard pageへ届くまで再帰で使い切る。
/// 一段ごとに`black_box`へ渡す配列がframeをstack上に残す。
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-kernel-stack"))]
#[allow(unconditional_recursion)]
#[inline(never)]
fn exhaust_kernel_stack(depth: usize) -> usize {
    let mut frame = [0u8; 256];
    frame[depth % frame.len()] = depth as u8;
    core::hint::black_box(&mut frame);
    exhaust_kernel_stack(depth + 1) + usize::from(frame[0])
}

#[cfg(target_arch = "riscv64")]
unsafe fn rust_user_trap_handler_impl(context: *mut UserContext) -> RunExit {
    let scause = arch::riscv64::csr::read_scause();
    let stval = arch::riscv64::csr::read_stval();
    // Safety: 呼び出し側の契約により有効なcontextである。
    let context = unsafe { &mut *context };
    match minios_kernel::user::trap::handle_user_trap(context, scause, stval) {
        TrapAction::SystemCall => user_trap_system_call(context),
        TrapAction::Timer => user_trap_timer(),
        // PLICのclaim/dispatchだけを済ませ、同じuser contextへそのまま戻す。
        TrapAction::External => {
            crate::drivers::plic::handle_external_pending();
            RunExit::Resume
        }
        TrapAction::Fatal { scause, stval } => user_trap_fatal(scause, stval),
    }
}

/// `ReadComplete`の受信済みbyteをguestへ届け、`a0`へ長さを書く。
/// copyは`copy_to_user`が`check_user_writable_range`と同じpage walkで
/// U/Wを再検査しながら物理frameへ書くため、検査経路と書き込み経路が
/// ずれない。S-modeからU pageへのSUM書き込みは不要になった。
#[cfg(target_arch = "riscv64")]
fn complete_user_read(
    context: &mut UserContext,
    space: &AddressSpace,
    start: u64,
    len: usize,
    data: &[u8],
) {
    // Safety: `USER_SYSCALL_PROBE_MEMORY`を指す共有参照はdispatchの
    // 呼び出しで返却済みであり、実行窓の排他契約で`&mut`へ変換できる。
    let memory = unsafe { &mut *(USER_SYSCALL_PROBE_MEMORY as *mut IdentityFrameStore) };
    complete_read(context, space, memory, start, len, data);
}

#[cfg(all(
    target_arch = "riscv64",
    not(any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit"
    ))
))]
fn user_trap_system_call(context: &mut UserContext) -> RunExit {
    // production payload path: dispatch staticsが設定されていない状態での
    // syscall (shell経由など) は起こり得ない。0なら早期に FAIL へ落とす。
    // Safety: 0比較のみで解参照しない。
    if unsafe { USER_SYSCALL_PROBE_SPACE } == 0 {
        crate::console::emergency_print(format_args!(
            "MiniOS user syscall without an active run\r\n"
        ));
        arch::riscv64::sbi::system_reset(
            arch::riscv64::sbi::ResetType::Shutdown,
            arch::riscv64::sbi::ResetReason::SystemFailure,
        );
    }
    // Safety: runnerが`__run_user`の直前に設定した単一hart静的参照である。
    // handlerの実行中はrunnerがassembly内で待機しているため、同時にaliasしない。
    let space = unsafe { &*(USER_SYSCALL_PROBE_SPACE as *const AddressSpace) };
    let memory = unsafe { &*(USER_SYSCALL_PROBE_MEMORY as *const IdentityFrameStore) };
    // Safety: 実行窓のhandlerが1 trapにつき1回だけ借り、外へ持ち出さない。
    let staging = unsafe { borrow_stdin_staging() };
    let mut source = control::UartControlSource::new(staging);
    let mut read_scratch = [0u8; minios_abi::syscall::MAX_READ_LEN];
    let flow = dispatch_syscall(
        context,
        space,
        memory,
        &mut control::UartControlSink,
        &mut source,
        &mut read_scratch,
    );
    match flow {
        SyscallFlow::Resume => RunExit::Resume,
        SyscallFlow::ReadComplete { start, len } => {
            complete_user_read(context, space, start, len, &read_scratch[..len]);
            RunExit::Resume
        }
        SyscallFlow::RecvComplete {
            start,
            len,
            addr_start,
        } => {
            // 先にsockaddrを書いてa0を上書きさせ、最後にpayload copyが
            // `a0 = len`を確定する。
            complete_user_read(
                context,
                space,
                addr_start,
                minios_abi::syscall::SOCKADDR_LEN,
                &read_scratch[len..len + minios_abi::syscall::SOCKADDR_LEN],
            );
            complete_user_read(context, space, start, len, &read_scratch[..len]);
            RunExit::Resume
        }
        SyscallFlow::Exit(code) => {
            USER_EXIT_CODE.store(code as usize, Ordering::Relaxed);
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_EXIT, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        SyscallFlow::Blocked => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_BLOCKED, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        SyscallFlow::Exec => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_EXEC, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        // `yield`/`sleep`はtimer preemptと同じoutcomeでkernelへ戻る。
        // `sleep`のcallerはcontrol層がsleep状態へmark済みである。
        SyscallFlow::Yield => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_PREEMPTED, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        SyscallFlow::Fatal(()) => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_SINK_FAILURE, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        SyscallFlow::SourceFatal(_) => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_SOURCE_FAILURE, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
    }
}

#[cfg(all(
    target_arch = "riscv64",
    not(any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit"
    ))
))]
fn user_trap_fatal(scause: usize, stval: usize) -> RunExit {
    // schedulerへ戻してからmanifestの終了契約に応じた診断を送る。
    // 複数imageのuser例外をGuestErrorにすると、hostが他のタスクまで停止してしまう。
    USER_FATAL_SCAUSE.store(scause, Ordering::Relaxed);
    USER_FATAL_STVAL.store(stval, Ordering::Relaxed);
    USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_FATAL_TRAP, Ordering::Relaxed);
    RunExit::ReturnToKernel
}

/// U-mode実行中のsupervisor timer割り込み。次tickを再アームしてからkernelへ
/// 戻り、schedulerが`Preempted`として中断processを再選対象へ戻す。
#[cfg(all(
    target_arch = "riscv64",
    not(any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit"
    ))
))]
fn user_trap_timer() -> RunExit {
    if let Err(error) = time::handle_interrupt() {
        // timerの再アーム失敗はタスクのuser例外ではなくkernel側の障害である。
        // 複数imageでも正常なタスク分離として扱わず、payload全体を失敗させる。
        crate::console::emergency_print(format_args!("MiniOS user timer: {error:?}\r\n"));
        USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_SOURCE_FAILURE, Ordering::Relaxed);
        return RunExit::ReturnToKernel;
    }
    USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_PREEMPTED, Ordering::Relaxed);
    RunExit::ReturnToKernel
}

/// probe経路は実行前にSTIEを落とすため、timer割り込みは到達しないはずの
/// 環境異常である。発生した場合はprobeの前提が崩れたとして失敗終了する。
#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit"
    )
))]
fn user_trap_timer() -> RunExit {
    crate::console::emergency_print(format_args!(
        "[MINIOS_TEST] failed: unexpected timer interrupt inside user probe\r\n"
    ));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-entry"))]
fn user_trap_system_call(_context: &mut UserContext) -> RunExit {
    // 分類を通過した時点で、このtrapがU-modeのecallであることは確定している。
    crate::println!("[MINIOS_TEST] user-entry: reached");
    RunExit::ReturnToKernel
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-trap"))]
fn user_trap_system_call(_context: &mut UserContext) -> RunExit {
    // このprobeのfixtureはecallを実行しない。到達したらharness側の欠陥である。
    crate::console::emergency_print(format_args!(
        "[MINIOS_TEST] failed: user-trap unexpected ecall\r\n"
    ));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-syscall"))]
fn user_trap_system_call(context: &mut UserContext) -> RunExit {
    // Safety: probe runnerが`__run_user`の前に設定した単一hart静的参照である。
    // handlerの実行中はrunnerがassembly内で待機しているため、同時にaliasしない。
    let space = unsafe { &*(USER_SYSCALL_PROBE_SPACE as *const AddressSpace) };
    let memory = unsafe { &*(USER_SYSCALL_PROBE_MEMORY as *const IdentityFrameStore) };
    // Safety: 実行窓のhandlerが1 trapにつき1回だけ借り、外へ持ち出さない。
    let staging = unsafe { borrow_stdin_staging() };
    let mut source = control::UartControlSource::new(staging);
    let mut read_scratch = [0u8; minios_abi::syscall::MAX_READ_LEN];
    let flow = dispatch_syscall(
        context,
        space,
        memory,
        &mut control::UartControlSink,
        &mut source,
        &mut read_scratch,
    );
    match flow {
        SyscallFlow::Resume => RunExit::Resume,
        SyscallFlow::ReadComplete { start, len } => {
            complete_user_read(context, space, start, len, &read_scratch[..len]);
            RunExit::Resume
        }
        SyscallFlow::RecvComplete {
            start,
            len,
            addr_start,
        } => {
            complete_user_read(
                context,
                space,
                addr_start,
                minios_abi::syscall::SOCKADDR_LEN,
                &read_scratch[len..len + minios_abi::syscall::SOCKADDR_LEN],
            );
            complete_user_read(context, space, start, len, &read_scratch[..len]);
            RunExit::Resume
        }
        SyscallFlow::Exit(code) => {
            // このprobeのExit経路はsinkへ直接frameを1回載せる。
            use minios_kernel::user::syscall::ControlSink as _;
            let mut sink = control::UartControlSink;
            let _ = sink.frame(minios_abi::control::FrameKind::Exit, &code.to_le_bytes());
            RunExit::ReturnToKernel
        }
        SyscallFlow::Blocked => {
            // probe fixtureはhost scriptが入力を同期済みであり、readで
            // 待つ設計ではない。到達したらharness側の欠陥である。
            crate::console::emergency_print(format_args!(
                "[MINIOS_TEST] failed: user-syscall blocked read\r\n"
            ));
            arch::riscv64::sbi::system_reset(
                arch::riscv64::sbi::ResetType::Shutdown,
                arch::riscv64::sbi::ResetReason::SystemFailure,
            )
        }
        SyscallFlow::Exec => {
            // probe fixtureはexecを呼ばない。到達したらharness側の欠陥。
            crate::console::emergency_print(format_args!(
                "[MINIOS_TEST] failed: user-syscall exec\r\n"
            ));
            arch::riscv64::sbi::system_reset(
                arch::riscv64::sbi::ResetType::Shutdown,
                arch::riscv64::sbi::ResetReason::SystemFailure,
            )
        }
        // probeにはschedulerがないため、自分へ戻るyieldとしてそのまま再開する。
        SyscallFlow::Yield => RunExit::Resume,
        SyscallFlow::Fatal(()) => {
            crate::console::emergency_print(format_args!(
                "[MINIOS_TEST] failed: user-syscall sink failure\r\n"
            ));
            arch::riscv64::sbi::system_reset(
                arch::riscv64::sbi::ResetType::Shutdown,
                arch::riscv64::sbi::ResetReason::SystemFailure,
            )
        }
        SyscallFlow::SourceFatal(_) => {
            crate::console::emergency_print(format_args!(
                "[MINIOS_TEST] failed: user-syscall source failure\r\n"
            ));
            arch::riscv64::sbi::system_reset(
                arch::riscv64::sbi::ResetType::Shutdown,
                arch::riscv64::sbi::ResetReason::SystemFailure,
            )
        }
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-exit"))]
fn user_trap_system_call(context: &mut UserContext) -> RunExit {
    // kernel-stack probeは本物のsyscall trapの中でtrap stackを使い切る。
    #[cfg(feature = "qemu-test-kernel-stack")]
    core::hint::black_box(exhaust_kernel_stack(0));
    // Safety: user-exit runnerが`__run_user`の直前に設定し、assemblyが
    // kernelへ戻るまで所有する単一hart静的参照である。
    let space = unsafe { &*(USER_SYSCALL_PROBE_SPACE as *const AddressSpace) };
    let memory = unsafe { &*(USER_SYSCALL_PROBE_MEMORY as *const IdentityFrameStore) };
    // Safety: 実行窓のhandlerが1 trapにつき1回だけ借り、外へ持ち出さない。
    let staging = unsafe { borrow_stdin_staging() };
    let mut source = control::UartControlSource::new(staging);
    let mut read_scratch = [0u8; minios_abi::syscall::MAX_READ_LEN];
    let flow = dispatch_syscall(
        context,
        space,
        memory,
        &mut control::UartControlSink,
        &mut source,
        &mut read_scratch,
    );
    match flow {
        SyscallFlow::Resume => RunExit::Resume,
        SyscallFlow::ReadComplete { start, len } => {
            complete_user_read(context, space, start, len, &read_scratch[..len]);
            RunExit::Resume
        }
        SyscallFlow::RecvComplete {
            start,
            len,
            addr_start,
        } => {
            complete_user_read(
                context,
                space,
                addr_start,
                minios_abi::syscall::SOCKADDR_LEN,
                &read_scratch[len..len + minios_abi::syscall::SOCKADDR_LEN],
            );
            complete_user_read(context, space, start, len, &read_scratch[..len]);
            RunExit::Resume
        }
        SyscallFlow::Exit(code) => {
            USER_EXIT_CODE.store(code as usize, Ordering::Relaxed);
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_EXIT, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        SyscallFlow::Blocked => {
            // このprobeのfixtureはreadを呼ばない。blocked readはharness欠陥。
            crate::console::emergency_print(format_args!(
                "[MINIOS_TEST] failed: user-exit blocked read\r\n"
            ));
            arch::riscv64::sbi::system_reset(
                arch::riscv64::sbi::ResetType::Shutdown,
                arch::riscv64::sbi::ResetReason::SystemFailure,
            )
        }
        SyscallFlow::Exec => {
            // このprobeのfixtureはexecを呼ばない。到達したらharness欠陥。
            crate::console::emergency_print(format_args!(
                "[MINIOS_TEST] failed: user-exit exec\r\n"
            ));
            arch::riscv64::sbi::system_reset(
                arch::riscv64::sbi::ResetType::Shutdown,
                arch::riscv64::sbi::ResetReason::SystemFailure,
            )
        }
        // probeにはschedulerがないため、自分へ戻るyieldとしてそのまま再開する。
        SyscallFlow::Yield => RunExit::Resume,
        SyscallFlow::Fatal(()) => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_SINK_FAILURE, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        SyscallFlow::SourceFatal(_) => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_SOURCE_FAILURE, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-entry"))]
fn user_trap_fatal(scause: usize, stval: usize) -> RunExit {
    crate::console::emergency_print(format_args!(
        "[MINIOS_TEST] failed: user-entry expected U-mode ecall, scause={scause:#018x} stval={stval:#018x}\r\n"
    ));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-trap"))]
fn user_trap_fatal(scause: usize, stval: usize) -> RunExit {
    const STORE_PAGE_FAULT: usize = 15;
    // fixtureがstoreを試すのはmachine記述が発見したUART pageである。
    let supervisor_uart_page = machine::spec().uart_base;
    if scause != STORE_PAGE_FAULT || stval != supervisor_uart_page {
        crate::console::emergency_print(format_args!(
            "[MINIOS_TEST] failed: user-trap expected supervisor UART denial, scause={scause:#018x} stval={stval:#018x}\r\n"
        ));
        arch::riscv64::sbi::system_reset(
            arch::riscv64::sbi::ResetType::Shutdown,
            arch::riscv64::sbi::ResetReason::SystemFailure,
        )
    }
    // 期待markerは独立した正確な一行で出す。成功marker (`user-trap: ok`) は
    // 決して出力しない。診断は次の行へ置く。
    crate::console::emergency_print(format_args!(
        "[MINIOS_TEST] user-trap: rejected\r\nMiniOS user trap: scause={scause:#018x} stval={stval:#018x}\r\n"
    ));
    // 拒否自体はこのprobeの成功条件であるため、正常shutdownでharnessへ検証を委ねる。
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-syscall"))]
fn user_trap_fatal(scause: usize, stval: usize) -> RunExit {
    if scause == 3 {
        // fixtureの全検査を通した後のebreak。成功markerを出して正常shutdownする。
        // 直前のUART出力はframe byte列 (改行を含まない) のため、markerを
        // 必ず新しい行から始める。
        crate::println!("\r\n[MINIOS_TEST] user-syscall: ok");
        successful_qemu_test_shutdown()
    }
    crate::console::emergency_print(format_args!(
        "[MINIOS_TEST] failed: user-syscall unexpected fatal, scause={scause:#018x} stval={stval:#018x}\r\n"
    ));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-exit"))]
fn user_trap_fatal(scause: usize, stval: usize) -> RunExit {
    USER_FATAL_SCAUSE.store(scause, Ordering::Relaxed);
    USER_FATAL_STVAL.store(stval, Ordering::Relaxed);
    USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_FATAL_TRAP, Ordering::Relaxed);
    RunExit::ReturnToKernel
}
