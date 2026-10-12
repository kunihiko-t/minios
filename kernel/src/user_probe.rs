#[cfg(any(
    feature = "qemu-test-user-entry",
    feature = "qemu-test-user-trap",
    feature = "qemu-test-user-syscall",
    feature = "qemu-test-user-exit"
))]
use super::*;
#[cfg(any(
    feature = "qemu-test-user-entry",
    feature = "qemu-test-user-trap",
    feature = "qemu-test-user-syscall",
    feature = "qemu-test-user-exit"
))]
use crate::user_trap::*;

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall"
    )
))]
const USER_PROBE_TRAP_STACK_BYTES: usize = 4 * PAGE_SIZE;

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall"
    )
))]
// Safety: このstaticのバイト列をRustは読み書きしない。`user.S`がsp相対で
// 固定header、UserContext、handler frameを置くための、4 KiB整列した専用test trap stackである。
#[repr(align(4096))]
struct UserProbeTrapStack([u8; USER_PROBE_TRAP_STACK_BYTES]);

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall"
    )
))]
static mut USER_PROBE_TRAP_STACK: UserProbeTrapStack =
    UserProbeTrapStack([0; USER_PROBE_TRAP_STACK_BYTES]);

#[cfg(all(
    target_arch = "riscv64",
    any(feature = "qemu-test-user-entry", feature = "qemu-test-user-trap")
))]
const USER_PROBE_CODE_LEN: usize = 12;

#[cfg(all(
    target_arch = "riscv64",
    any(feature = "qemu-test-user-entry", feature = "qemu-test-user-trap")
))]
const USER_PROBE_ELF_LEN: usize = 0x1000 + USER_PROBE_CODE_LEN;

#[cfg(all(
    target_arch = "riscv64",
    any(feature = "qemu-test-user-entry", feature = "qemu-test-user-trap")
))]
// 決定的な1 segment ELFを返す。textは12 byteのコード領域だけで、
// 中身は呼び出し側のprobeが指定する。
fn user_probe_elf(code: &[u8; USER_PROBE_CODE_LEN]) -> [u8; USER_PROBE_ELF_LEN] {
    let mut bytes = [0u8; USER_PROBE_ELF_LEN];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&243u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x0010_0000u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    let header = 64;
    bytes[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
    bytes[header + 4..header + 8].copy_from_slice(&5u32.to_le_bytes());
    bytes[header + 8..header + 16].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[header + 16..header + 24].copy_from_slice(&0x0010_0000u64.to_le_bytes());
    bytes[header + 32..header + 40].copy_from_slice(&(USER_PROBE_CODE_LEN as u64).to_le_bytes());
    bytes[header + 40..header + 48].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[header + 48..header + 56].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[0x1000..0x1000 + USER_PROBE_CODE_LEN].copy_from_slice(code);
    bytes
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-entry"))]
// textは`ecall`一命令と自己ループだけで、trap handlerへ到達できたこと以外を表明しない。
fn user_entry_ecall_elf() -> [u8; USER_PROBE_ELF_LEN] {
    user_probe_elf(&[
        0x73, 0x00, 0x00, 0x00, // ecall
        0x6f, 0x00, 0x00, 0x00, // jal x0, 0
        0x6f, 0x00, 0x00, 0x00, // jal x0, 0
    ])
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-trap"))]
// textはU=0でmapされたUART page 0x1000_0000へのstoreでpage fault (原因15)を
// 起こす。storeが通れば自己ループで停止し、harnessのtimeoutが失敗を報告する。
fn user_trap_fault_elf() -> [u8; USER_PROBE_ELF_LEN] {
    user_probe_elf(&[
        0xb7, 0x02, 0x00, 0x10, // lui t0, 0x10000
        0x23, 0xb0, 0x02, 0x00, // sd x0, 0(t0)
        0x6f, 0x00, 0x00, 0x00, // jal x0, 0
    ])
}

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall"
    )
))]
pub(crate) fn run_user_mode_probe_test(
    kernel_space: &AddressSpace,
    plan: &KernelMapPlan,
    frames: &mut dyn FrameSource,
    memory: &mut IdentityFrameStore,
) -> ! {
    #[cfg(feature = "qemu-test-user-entry")]
    let fixture = user_entry_ecall_elf();
    #[cfg(feature = "qemu-test-user-trap")]
    let fixture = user_trap_fault_elf();
    #[cfg(feature = "qemu-test-user-syscall")]
    let fixture = user_syscall_probe_elf();
    let image = match load_image_with_kernel_mappings(&fixture, frames, memory, plan.mappings()) {
        Ok(image) => image,
        Err(error) => fatal_qemu_test(format_args!("user probe load: {error:?}")),
    };

    let mut context = UserContext::new(image.entry(), image.user_stack_top());
    let user_root = PhysPageNum::from_start(image.address_space().root().as_u64())
        .expect("user root page number is valid");
    let kernel_root = PhysPageNum::from_start(kernel_space.root().as_u64())
        .expect("kernel root page number is valid");
    // Safety: アドレスを作るだけでstaticのバイト列を読み書きしない。
    let trap_stack_top = unsafe {
        core::ptr::addr_of!(USER_PROBE_TRAP_STACK.0) as usize + USER_PROBE_TRAP_STACK_BYTES
    };

    #[cfg(feature = "qemu-test-user-syscall")]
    {
        // Safety: 参照を作るだけで解参照しない。handlerはprobe実行中だけ使う。
        unsafe {
            USER_SYSCALL_PROBE_SPACE = image.address_space() as *const _ as usize;
            USER_SYSCALL_PROBE_MEMORY = memory as *const IdentityFrameStore as usize;
        }
    }

    // Supervisor timer割り込みは分類上Fatalとなる。probeはticksに依存しないため、
    // 実行窓をtimerの非決定性から切り離してからU-modeへ降りる (STIE=bit 5)。
    const SIE_STIE: usize = 1 << 5;
    // Safety: S-modeで`sie`のSupervisor timer許可bitだけを落とす。
    unsafe {
        let sie = arch::riscv64::csr::read_sie();
        arch::riscv64::csr::write_sie(sie & !SIE_STIE);
    }

    // このprobeの間だけstvecをuser trap入口へ向ける。通常のS-mode入口は
    // `trap::init`が設定したものであり、user trapとは混同しない。
    unsafe { arch::riscv64::csr::write_stvec(__user_trap_entry as *const () as usize) };

    // Safety: `context`はこのframeの実引数であり、`__run_user`が返るまで
    // user address spaceの所有権はassembly側にある。両satpは直前に検証した
    // 有効なSv39 rootであり、trap stack topは4 KiB整列した専用staticの上端である。
    let exit = unsafe {
        __run_user(
            &raw mut context,
            arch::riscv64::csr::sv39_satp_bits(user_root),
            arch::riscv64::csr::sv39_satp_bits(kernel_root),
            trap_stack_top,
        )
    };

    #[cfg(feature = "qemu-test-user-entry")]
    {
        if exit != RunExit::ReturnToKernel {
            fatal_qemu_test(format_args!(
                "user-entry exit: expected ReturnToKernel, actual {exit:?}"
            ));
        }
        successful_qemu_test_shutdown()
    }

    #[cfg(feature = "qemu-test-user-trap")]
    {
        // このfeatureのhandlerはfatal trapを検証した時点でshutdownする。
        // ここへ戻ったということは、拒否すべきtrapからkernelが復帰してしまった。
        fatal_qemu_test(format_args!(
            "user-trap: kernel resumed after the rejected trap, exit={exit:?}"
        ))
    }

    #[cfg(feature = "qemu-test-user-syscall")]
    {
        // handlerは検査の成否にかかわらずkernel内でshutdownする。
        // 戻ってきた場合は想定外のRunExitである。
        fatal_qemu_test(format_args!(
            "user-syscall: kernel resumed unexpectedly, exit={exit:?}"
        ))
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-exit"))]
pub(crate) fn run_user_exit_test(
    kernel_space: &AddressSpace,
    plan: &KernelMapPlan,
    frames: &mut dyn FrameSource,
    memory: &mut IdentityFrameStore,
) {
    let before = frames.stats();
    let fixture = user_exit_probe_elf();
    let image = match load_image_with_kernel_mappings(&fixture, frames, memory, plan.mappings()) {
        Ok(image) => image,
        Err(error) => fatal_qemu_test(format_args!("user-exit load: {error:?}")),
    };
    let mut context = UserContext::new(image.entry(), image.user_stack_top());
    let kernel_root = PhysPageNum::from_start(kernel_space.root().as_u64())
        .expect("kernel root page number is valid");
    let mut run = match UserRun::new(image, frames, memory, kernel_root) {
        Ok(run) => run,
        Err(error) => fatal_qemu_test(format_args!("user-exit run build: {error:?}")),
    };

    USER_EXIT_CODE.store(usize::MAX, Ordering::Relaxed);
    USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_NONE, Ordering::Relaxed);
    USER_FATAL_SCAUSE.store(0, Ordering::Relaxed);
    USER_FATAL_STVAL.store(0, Ordering::Relaxed);
    // Safety: pointer値を保存するだけでここでは解参照しない。handlerだけが
    // assemblyの実行窓で読み、kernelへ戻った直後に両方をclearする。
    unsafe {
        USER_SYSCALL_PROBE_SPACE = run.address_space() as *const _ as usize;
        USER_SYSCALL_PROBE_MEMORY = run.memory() as *const IdentityFrameStore as usize;
        USER_STDIN_STAGING = StdinStaging::new();
    }

    // probeはtimer tickに依存せず、Supervisor timer割り込みはuser trapの
    // 分類上Fatalなので、実行窓だけSTIEを無効化する。
    const SIE_STIE: usize = 1 << 5;
    // Safety: S-modeで`sie`のSupervisor timer許可bitだけを落とす。
    unsafe {
        let sie = arch::riscv64::csr::read_sie();
        arch::riscv64::csr::write_sie(sie & !SIE_STIE);
        arch::riscv64::csr::write_stvec(__user_trap_entry as *const () as usize);
    }

    let mut assembly_exit = RunExit::Resume;
    let mut sink = control::UartControlSink;
    let completion = run.execute(&mut sink, |launch| {
        // Safety: runが両address space、frame memory、連続した専用trap stackを
        // 所有する。assemblyはkernel satpとboot stackを復元してから戻る。
        assembly_exit = unsafe {
            __run_user(
                &raw mut context,
                launch.user_satp(),
                launch.kernel_satp(),
                launch.kernel_stack_top(),
            )
        };
        if assembly_exit != RunExit::ReturnToKernel {
            return RunOutcome::Fatal;
        }
        match USER_RUN_OUTCOME.load(Ordering::Relaxed) {
            USER_RUN_OUTCOME_EXIT => {
                RunOutcome::Exit(USER_EXIT_CODE.load(Ordering::Relaxed) as u32)
            }
            USER_RUN_OUTCOME_FATAL_TRAP
            | USER_RUN_OUTCOME_SINK_FAILURE
            | USER_RUN_OUTCOME_SOURCE_FAILURE => RunOutcome::Fatal,
            _ => RunOutcome::Fatal,
        }
    });
    // Safety: handlerが今後走らないkernel側へ戻ったため、danglingになり得る
    // pointer値を解放前に無効化する。
    unsafe {
        USER_SYSCALL_PROBE_SPACE = 0;
        USER_SYSCALL_PROBE_MEMORY = 0;
    }

    let outcome_kind = USER_RUN_OUTCOME.load(Ordering::Relaxed);
    let completion = match completion {
        Ok(completion) => completion,
        Err(error) => {
            drop(run);
            fatal_qemu_test(format_args!("user-exit finish: {error:?}"));
        }
    };
    drop(run);

    let after = frames.stats();
    if after != before {
        fatal_qemu_test(format_args!(
            "user-exit recovery: allocator expected={before:?} actual={after:?}"
        ));
    }

    match completion {
        RunCompletion::Exit(42) => {
            // 直前のstdout、stderr、Exitは改行を含まないbinary frameなので、
            // markerを独立した一行として始める。
            crate::println!("\r\n[MINIOS_TEST] user-exit: ok code=42");
            successful_qemu_test_shutdown()
        }
        RunCompletion::Exit(code) => {
            fatal_qemu_test(format_args!("user-exit code: expected 42, actual {code}"))
        }
        RunCompletion::Fatal if assembly_exit != RunExit::ReturnToKernel => fatal_qemu_test(
            format_args!("user-exit return: expected ReturnToKernel, actual {assembly_exit:?}"),
        ),
        RunCompletion::Fatal if outcome_kind == USER_RUN_OUTCOME_SINK_FAILURE => {
            fatal_qemu_test(format_args!("user-exit sink failure after cleanup"))
        }
        RunCompletion::Fatal if outcome_kind == USER_RUN_OUTCOME_SOURCE_FAILURE => {
            fatal_qemu_test(format_args!("user-exit source failure after cleanup"))
        }
        RunCompletion::Fatal if outcome_kind == USER_RUN_OUTCOME_FATAL_TRAP => {
            fatal_qemu_test(format_args!(
                "user-exit fatal after cleanup: scause={:#018x} stval={:#018x}",
                USER_FATAL_SCAUSE.load(Ordering::Relaxed),
                USER_FATAL_STVAL.load(Ordering::Relaxed)
            ))
        }
        RunCompletion::Fatal => fatal_qemu_test(format_args!(
            "user-exit returned without an outcome after cleanup"
        )),
    }
}
