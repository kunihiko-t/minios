#![no_std]
#![no_main]

#[cfg(any(
    all(
        feature = "qemu-test-boot",
        any(
            feature = "qemu-test-timer",
            feature = "qemu-test-trap",
            feature = "qemu-test-memory",
            feature = "qemu-test-vm",
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit"
        )
    ),
    all(
        feature = "qemu-test-timer",
        any(
            feature = "qemu-test-trap",
            feature = "qemu-test-memory",
            feature = "qemu-test-vm",
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit"
        )
    ),
    all(
        feature = "qemu-test-trap",
        any(
            feature = "qemu-test-memory",
            feature = "qemu-test-vm",
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit"
        )
    ),
    all(
        feature = "qemu-test-memory",
        any(
            feature = "qemu-test-vm",
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit"
        )
    ),
    all(
        feature = "qemu-test-vm",
        any(
            feature = "qemu-test-elf",
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit"
        )
    ),
    all(
        feature = "qemu-test-elf",
        any(
            feature = "qemu-test-user-entry",
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit"
        )
    ),
    all(
        feature = "qemu-test-user-entry",
        any(
            feature = "qemu-test-user-trap",
            feature = "qemu-test-user-syscall",
            feature = "qemu-test-user-exit"
        )
    ),
    all(
        feature = "qemu-test-user-trap",
        any(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit")
    ),
    all(feature = "qemu-test-user-syscall", feature = "qemu-test-user-exit")
))]
compile_error!("QEMU kernel test features are mutually exclusive; enable at most one");

#[cfg(target_arch = "riscv64")]
mod arch;
#[cfg(target_arch = "riscv32")]
mod arch;
#[cfg(target_arch = "riscv64")]
mod console;
#[cfg(target_arch = "riscv32")]
mod console;
#[cfg(target_arch = "riscv64")]
mod drivers;
#[cfg(target_arch = "riscv32")]
mod drivers;
#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
mod shell;
#[cfg(target_arch = "riscv64")]
mod time;

#[cfg(target_arch = "riscv64")]
mod control;

#[cfg(target_arch = "riscv64")]
use core::panic::PanicInfo;
#[cfg(target_arch = "riscv32")]
use core::panic::PanicInfo;
#[cfg(target_arch = "riscv64")]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_arch = "riscv32")]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_arch = "riscv64")]
use minios_kernel::boot_payload::BootPayload;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-exit"))]
use minios_kernel::elf::fixture::user_exit_probe_elf;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-user-syscall"))]
use minios_kernel::elf::fixture::user_syscall_probe_elf;
#[cfg(target_arch = "riscv64")]
use minios_kernel::elf::load::load_image_with_kernel_mappings;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
use minios_kernel::memory::frame::{FrameStats, PhysFrame};
#[cfg(target_arch = "riscv64")]
use minios_kernel::memory::{
    BOOT_PAYLOAD_START, KernelSections, PHYSICAL_MEMORY_END,
    frame::{FrameAllocator, FrameError, PAGE_SIZE},
};
#[cfg(target_arch = "riscv64")]
use minios_kernel::user::run::{RunCompletion, RunOutcome, UserRun};
#[cfg(target_arch = "riscv64")]
use minios_kernel::user::syscall::{SyscallFlow, dispatch_syscall};
#[cfg(target_arch = "riscv64")]
use minios_kernel::user::trap::TrapAction;
#[cfg(target_arch = "riscv64")]
use minios_kernel::user::{RunExit, UserContext};
#[cfg(target_arch = "riscv64")]
use minios_kernel::vm::AddressSpace;
#[cfg(target_arch = "riscv64")]
use minios_kernel::vm::{
    AddressSpaceBuilder, AddressSpaceStorage, IdentityFrameStore, KernelMapPlan, PhysPageNum,
};
#[cfg(all(
    target_arch = "riscv64",
    any(feature = "qemu-test-vm", feature = "qemu-test-elf")
))]
use minios_kernel::vm::{VirtAddr, VmError};
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
use minios_kernel::{
    elf::{
        LoadedImage, USER_GUARD_BOTTOM, USER_STACK_BOTTOM, fixture::valid_riscv64_elf, load_image,
    },
    vm::{FrameStore, IdentityFrameStoreError, PageFlags, PhysAddr},
};

#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
const UNKNOWN_HART_ID: usize = usize::MAX;
#[cfg(any(target_arch = "riscv64", target_arch = "riscv32"))]
static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(UNKNOWN_HART_ID);

#[cfg(target_arch = "riscv64")]
static mut KERNEL_ADDRESS_SPACE_STORAGE: AddressSpaceStorage<2688> = AddressSpaceStorage::new();

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
static mut ELF_ADDRESS_SPACE_STORAGE: AddressSpaceStorage<2688> = AddressSpaceStorage::new();

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit"
    )
))]
static mut USER_PROBE_ADDRESS_SPACE_STORAGE: AddressSpaceStorage<2688> = AddressSpaceStorage::new();

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

// handlerへ渡す実行中imageのaddress spaceとframe store。test probeと
// production payload pathの両方が使う。
// Safety: `__run_user`の直前にrunnerが設定し、handlerだけが解参照する。
// kernelへ戻った直後にrunnerが0へ戻す。
// user-mode実行基盤はRV64のQEMU運用の仕組みであり、RV32実機では使わない。
#[cfg(target_arch = "riscv64")]
static mut USER_SYSCALL_PROBE_SPACE: usize = 0;
#[cfg(target_arch = "riscv64")]
static mut USER_SYSCALL_PROBE_MEMORY: usize = 0;

#[cfg(target_arch = "riscv64")]
static mut PAYLOAD_ADDRESS_SPACE_STORAGE: AddressSpaceStorage<2688> = AddressSpaceStorage::new();

#[cfg(target_arch = "riscv64")]
static USER_EXIT_CODE: AtomicUsize = AtomicUsize::new(usize::MAX);
#[cfg(target_arch = "riscv64")]
static USER_RUN_OUTCOME: AtomicUsize = AtomicUsize::new(USER_RUN_OUTCOME_NONE);
#[cfg(target_arch = "riscv64")]
static USER_FATAL_SCAUSE: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_arch = "riscv64")]
static USER_FATAL_STVAL: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_NONE: usize = 0;
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_EXIT: usize = 1;
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_FATAL_TRAP: usize = 2;
#[cfg(target_arch = "riscv64")]
const USER_RUN_OUTCOME_SINK_FAILURE: usize = 3;

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
const ELF_FIXTURE_OWNED_FRAMES: usize = 23;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
const ELF_DIRTY_FRAME_COUNT: usize = 64;
#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
const _: () = assert!(ELF_DIRTY_FRAME_COUNT >= ELF_FIXTURE_OWNED_FRAMES);

#[cfg(target_arch = "riscv64")]
// Safety: `linker.ld`がRISC-Vカーネルイメージの末尾に必ず定義するC ABIシンボルである。
// Rust側は`addr_of!`でアドレスを作るだけで、外部staticの内容を読み書きしない。
unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __bss_end: u8;
    static __boot_stack_start: u8;
    static __boot_stack_end: u8;
    static __kernel_end: u8;
}

#[cfg(target_arch = "riscv64")]
// `entry.S`は、外部C ABIシンボル`kernel_main`を呼び出す。
// OpenSBIの`a0/a1`を`hart_id/dtb`としてそのまま受け取るため、この名前とABIを変えてはならない。
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(hart_id: usize, dtb: usize) -> ! {
    // パニック診断が起動ハートを識別できるよう、ほかの初期化より前に記録する。
    BOOT_HART_ID.store(hart_id, Ordering::Relaxed);
    // ターゲット固有のコンソール初期化。QEMUでは何もしない。
    console::init();
    // DTB は後のハードウェア検出で使うまで保持する OpenSBI の ABI 引数である。
    let _ = dtb;
    arch::riscv64::trap::init();
    crate::println!("[ok] traps");

    let managed_memory_start = kernel_memory_start();
    // Safety: OpenSBIが使う`0x8000_0000..0x8020_0000`と、リンカーが配置する
    // `0x8020_0000..managed_memory_start`のカーネルイメージを除外している。
    // boot payloadの開始`0x8780_0000`までを所有するのは、この局所アロケーターだけである。
    // このアロケーターが生存している間は、同じ範囲を管理する別の所有者を作らない。
    let mut frames =
        match unsafe { FrameAllocator::<512>::new(managed_memory_start, PHYSICAL_MEMORY_END) } {
            Ok(frames) => frames,
            Err(error) => fatal_memory_error(error),
        };

    let sections = kernel_sections();
    // 予約窓はこの時点ではまだaddress spaceをactivateしていないbare mode
    // (VA==PA) にあるため、物理addressから直接検証できる。loaderがpayloadを
    // 置いていない場合はNoneとなり、shellへ抜ける。
    let payload = if unsafe { BootPayload::reserved_window_has_bundle() } {
        // Safety: `-m 128M`と`-device loader`が予約窓を有効RAMとして配置する。
        match unsafe { BootPayload::from_reserved_window() } {
            Ok(payload) => Some(payload),
            Err(error) => fatal_payload_error(format_args!(
                "MiniOS payload: invalid bundle, {error:?}\r\n"
            )),
        }
    } else {
        None
    };
    let plan = match KernelMapPlan::new(&sections, managed_memory_start, PHYSICAL_MEMORY_END) {
        Ok(plan) => plan,
        Err(error) => panic!("invalid kernel mapping plan: {error:?}"),
    };
    // payloadが存在するときだけ、使用page (切り上げ) をS-mode read-onlyで
    // kernel空間とuser空間のborrowed mappingへ加える。全8 MiBはmapしない。
    let plan = match payload.as_ref() {
        Some(payload) => plan
            .with_payload_pages(BOOT_PAYLOAD_START, payload.total_len() as usize)
            .unwrap_or_else(|error| panic!("invalid payload mapping plan: {error:?}")),
        None => plan,
    };
    // Safety: QEMU virt exposes managed_memory_start..PHYSICAL_MEMORY_END as
    // valid RAM. Bare translation reaches it by identity before satp changes,
    // and `plan` identity-maps the complete range afterward. This boot hart
    // creates the only IdentityFrameStore, and all byte access to allocator-
    // issued frames goes through it for the rest of kernel_main. The allocator
    // only tracks/assigns frames and never dereferences their memory itself.
    let mut memory =
        match unsafe { IdentityFrameStore::new(managed_memory_start, PHYSICAL_MEMORY_END) } {
            Ok(memory) => memory,
            Err(error) => panic!("invalid identity frame-store range: {error:?}"),
        };
    // Safety: this is the boot hart's sole access to the static ownership
    // table, and kernel_main never returns or constructs another mutable
    // reference to it.
    let storage_pointer = &raw mut KERNEL_ADDRESS_SPACE_STORAGE;
    let storage = unsafe { storage_pointer.as_mut() }
        .expect("a static address-space storage pointer is never null");
    let mut builder = match AddressSpaceBuilder::new(&mut frames, &mut memory, storage) {
        Ok(builder) => builder,
        Err(error) => panic!("kernel address-space root allocation failed: {error:?}"),
    };
    for mapping in plan.mappings() {
        if let Err(error) =
            builder.map_borrowed(mapping.page(), mapping.physical(), mapping.flags())
        {
            panic!("kernel identity mapping failed: {error:?}");
        }
    }
    let kernel_space = builder.finish();
    let root = match PhysPageNum::from_start(kernel_space.root().as_u64()) {
        Ok(root) => root,
        Err(error) => panic!("kernel root page number is invalid: {error:?}"),
    };
    // Safety: the completed plan identity-maps the executing kernel text, boot
    // stack, trap vector, UART, and allocator-managed RAM that owns every page
    // table. `root` belongs to this address space and remains live forever.
    unsafe { arch::riscv64::csr::activate_sv39(root) };

    if let Err(error) = time::init() {
        fatal_timer_error("initial schedule", error);
    }
    crate::println!("[ok] timer");
    crate::println!("[ok] memory");

    #[cfg(feature = "qemu-test-vm")]
    {
        run_vm_test(&kernel_space, &memory);
    }

    #[cfg(any(
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall"
    ))]
    {
        run_user_mode_probe_test(&kernel_space, &plan, &mut frames, &mut memory);
    }

    #[cfg(feature = "qemu-test-user-exit")]
    {
        run_user_exit_test(&kernel_space, &plan, &mut frames, &mut memory);
    }

    #[cfg(feature = "qemu-test-elf")]
    {
        run_elf_test(&kernel_space, &mut frames, &mut memory);
    }

    #[cfg(feature = "qemu-test-memory")]
    {
        run_memory_test(&mut frames);
    }

    if let Some(payload) = payload {
        run_boot_payload(&kernel_space, &plan, &mut frames, &mut memory, payload);
    }

    crate::println!("MiniOS booting...");
    crate::println!("hart id: {hart_id}");

    #[cfg(feature = "qemu-test-boot")]
    {
        crate::println!("[MINIOS_TEST] boot: ok");
        arch::riscv64::sbi::system_reset(
            arch::riscv64::sbi::ResetType::Shutdown,
            arch::riscv64::sbi::ResetReason::NoReason,
        );
    }

    #[cfg(feature = "qemu-test-trap")]
    {
        // Safety: `ebreak`はメモリーとスタックを変更せず、同期ブレークポイント例外を発生させる。
        // `trap::init`は完了しており、ハンドラーがテスト成功としてリセットする規約になっている。
        unsafe { core::arch::asm!("ebreak", options(nomem, nostack)) };
    }

    #[cfg(feature = "qemu-test-timer")]
    {
        while time::ticks() < 3 {
            core::hint::spin_loop();
        }
        crate::println!("[MINIOS_TEST] timer: ok");
        arch::riscv64::sbi::system_reset(
            arch::riscv64::sbi::ResetType::Shutdown,
            arch::riscv64::sbi::ResetReason::NoReason,
        );
    }

    shell::run(hart_id, &mut frames)
}

#[cfg(target_arch = "riscv64")]
fn kernel_memory_start() -> usize {
    // `__kernel_end`は、`linker.ld`がカーネルイメージの実在する境界として定義する。
    // `addr_of!`はアドレスを作るだけで、外部staticのメモリー内容を読み取らない。
    let kernel_end = core::ptr::addr_of!(__kernel_end) as usize;
    // `linker.ld`はこのシンボルを4 KiB境界にそろえるが、将来リンカーを変更してもイメージを保護できるよう、ここでも上向きに丸める。
    align_up_to_page(kernel_end)
}

#[cfg(target_arch = "riscv64")]
fn kernel_sections() -> KernelSections {
    let text = core::ptr::addr_of!(__text_start) as usize..core::ptr::addr_of!(__text_end) as usize;
    let rodata =
        core::ptr::addr_of!(__rodata_start) as usize..core::ptr::addr_of!(__rodata_end) as usize;
    let writable =
        core::ptr::addr_of!(__data_start) as usize..core::ptr::addr_of!(__bss_end) as usize;
    let boot_stack = core::ptr::addr_of!(__boot_stack_start) as usize
        ..core::ptr::addr_of!(__boot_stack_end) as usize;
    let kernel_end = core::ptr::addr_of!(__kernel_end) as usize;

    match KernelSections::new(text, rodata, writable, boot_stack, kernel_end) {
        Ok(sections) => sections,
        Err(error) => panic!("invalid linker-provided kernel sections: {error:?}"),
    }
}

#[cfg(target_arch = "riscv64")]
fn align_up_to_page(address: usize) -> usize {
    // カーネルイメージは`0x8800_0000`より十分低い位置にあるため、`PAGE_SIZE - 1`を加えても`usize`を超えない。
    (address + (PAGE_SIZE - 1)) & !(PAGE_SIZE - 1)
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-vm"))]
fn run_vm_test<const N: usize>(kernel_space: &AddressSpace<'_, N>, memory: &IdentityFrameStore) {
    expect_kernel_range(
        kernel_space,
        memory,
        ".text",
        core::ptr::addr_of!(__text_start) as usize,
        core::ptr::addr_of!(__text_end) as usize,
        (true, false, true, false),
    );
    expect_kernel_range(
        kernel_space,
        memory,
        ".rodata",
        core::ptr::addr_of!(__rodata_start) as usize,
        core::ptr::addr_of!(__rodata_end) as usize,
        (true, false, false, false),
    );
    expect_kernel_range(
        kernel_space,
        memory,
        ".data/.bss",
        core::ptr::addr_of!(__data_start) as usize,
        core::ptr::addr_of!(__bss_end) as usize,
        (true, true, false, false),
    );
    expect_kernel_range(
        kernel_space,
        memory,
        "boot stack",
        core::ptr::addr_of!(__boot_stack_start) as usize,
        core::ptr::addr_of!(__boot_stack_end) as usize,
        (true, true, false, false),
    );
    expect_kernel_range(
        kernel_space,
        memory,
        "UART",
        0x1000_0000,
        0x1000_0000 + PAGE_SIZE,
        (true, true, false, false),
    );

    let payload = VirtAddr::try_new(0x8780_0000).expect("payload start is an Sv39 address");
    let actual = kernel_space.translate(memory, payload);
    if !matches!(actual, Err(VmError::NotMapped)) {
        fatal_qemu_test(format_args!(
            "vm mapping payload @ {:#x}: expected NotMapped, actual {actual:?}",
            payload.as_u64()
        ));
    }

    crate::println!("[MINIOS_TEST] vm: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-vm"))]
fn expect_kernel_range<const N: usize>(
    kernel_space: &AddressSpace<'_, N>,
    memory: &IdentityFrameStore,
    region: &str,
    start: usize,
    end: usize,
    expected_flags: (bool, bool, bool, bool),
) {
    if start >= end || !start.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) {
        fatal_qemu_test(format_args!(
            "vm range {region}: expected nonempty page-aligned [start,end), actual [{start:#x},{end:#x})"
        ));
    }
    let expected_pages = (end - start) / PAGE_SIZE;
    let mut checked_pages = 0usize;
    for address in (start..end).step_by(PAGE_SIZE) {
        let virtual_address = VirtAddr::try_new(address as u64)
            .expect("linker and MMIO addresses are valid Sv39 values");
        let actual = kernel_space.translate(memory, virtual_address);
        let matches = matches!(
            actual,
            Ok((physical, flags))
                if physical.as_u64() == address as u64
                    && (flags.read(), flags.write(), flags.execute(), flags.user())
                        == expected_flags
        );
        if !matches {
            fatal_qemu_test(format_args!(
                "vm range {region} page {checked_pages}/{expected_pages} @ {address:#x} in [{start:#x},{end:#x}): expected identity physical={address:#x} flags(R,W,X,U)={expected_flags:?}, actual {actual:?}"
            ));
        }
        checked_pages += 1;
    }
    let observed_end = start + checked_pages * PAGE_SIZE;
    if checked_pages != expected_pages || observed_end != end {
        fatal_qemu_test(format_args!(
            "vm range {region}: expected {expected_pages} pages ending exclusively at {end:#x}, actual {checked_pages} pages ending at {observed_end:#x}"
        ));
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn run_elf_test<const N: usize>(
    kernel_space: &AddressSpace<'_, N>,
    frames: &mut FrameAllocator<512>,
    memory: &mut IdentityFrameStore,
) {
    let before = frames.stats();
    // Safety: this feature path runs only on the single boot hart, obtains the
    // static once, and destroys the loaded image before inspecting it again.
    let storage_pointer = &raw mut ELF_ADDRESS_SPACE_STORAGE;
    let storage = unsafe { storage_pointer.as_mut() }
        .expect("a static ELF address-space storage pointer is never null");
    if !storage.is_empty() {
        fatal_qemu_test(format_args!(
            "elf precondition: expected empty storage, actual len={}",
            storage.len()
        ));
    }

    dirty_reusable_elf_frames(frames, memory, before);
    if frames.stats() != before || !storage.is_empty() {
        fatal_qemu_test(format_args!(
            "elf dirty-frame recovery: allocator expected={before:?} actual={:?}; storage expected len=0 actual len={}",
            frames.stats(),
            storage.len()
        ));
    }

    // The deterministic fixture is 8,196 bytes. It is intentionally the only
    // large automatic value here; the much larger 2,688-entry ownership table
    // above resides in static kernel storage, not on the 64 KiB boot stack.
    let fixture = valid_riscv64_elf();
    let image = match load_image(&fixture, frames, memory, storage) {
        Ok(image) => image,
        Err(error) => {
            fatal_qemu_test(format_args!(
                "elf load: {error:?}; allocator expected={before:?} actual={:?}; storage len={}",
                frames.stats(),
                storage.len()
            ));
        }
    };

    let inspection = inspect_loaded_elf(&image, kernel_space, memory);
    if let Err(error) = image.destroy(frames) {
        fatal_qemu_test(format_args!(
            "elf destroy: {:?}; inspection={inspection:?}; allocator expected={before:?} actual={:?}",
            error.frame_error(),
            frames.stats()
        ));
    }

    let after = frames.stats();
    let storage_len = storage.len();
    if let Err(error) = inspection {
        fatal_qemu_test(format_args!(
            "elf inspection: {error:?}; recovery allocator expected={before:?} actual={after:?}; storage len={storage_len}"
        ));
    }
    if after != before {
        fatal_qemu_test(format_args!(
            "elf recovery allocator: expected={before:?}, actual={after:?}; storage len={storage_len}"
        ));
    }
    if storage_len != 0 {
        fatal_qemu_test(format_args!(
            "elf recovery storage: expected len=0, actual len={storage_len}; allocator={after:?}"
        ));
    }

    crate::println!("[MINIOS_TEST] elf: ok");
    successful_qemu_test_shutdown()
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn dirty_reusable_elf_frames(
    frames: &mut FrameAllocator<512>,
    memory: &mut IdentityFrameStore,
    baseline: FrameStats,
) {
    let mut dirty_frames: [Option<PhysFrame>; ELF_DIRTY_FRAME_COUNT] =
        [const { None }; ELF_DIRTY_FRAME_COUNT];
    let pattern = [0xa5; 64];

    // FrameAllocator::allocate scans its bitmap from index zero. Holding the
    // lowest 64 free frames, dirtying them, and then releasing all of them
    // makes the next 23 fixture allocations reuse dirty frames before any
    // untouched frame. The host characterization test fixes that 23-frame
    // requirement (five page tables, two segments, and sixteen stack pages).
    for index in 0..ELF_DIRTY_FRAME_COUNT {
        let frame = match frames.allocate() {
            Some(frame) => frame,
            None => {
                let cleanup_error = release_dirty_elf_frames(frames, &mut dirty_frames);
                fatal_qemu_test(format_args!(
                    "elf dirty-frame allocation {index}/{ELF_DIRTY_FRAME_COUNT}: out of frames; cleanup={cleanup_error:?}; allocator expected={baseline:?} actual={:?}",
                    frames.stats()
                ));
            }
        };
        let frame_start = frame.start();
        dirty_frames[index] = Some(frame);
        for offset in (0..PAGE_SIZE).step_by(pattern.len()) {
            if let Err(error) = memory.copy_into(frame_start, offset, &pattern) {
                let cleanup_error = release_dirty_elf_frames(frames, &mut dirty_frames);
                fatal_qemu_test(format_args!(
                    "elf dirty-frame write {index}/{ELF_DIRTY_FRAME_COUNT} frame={frame_start:#x} offset={offset:#x}: {error:?}; cleanup={cleanup_error:?}; allocator expected={baseline:?} actual={:?}",
                    frames.stats()
                ));
            }
        }
    }

    if let Some(error) = release_dirty_elf_frames(frames, &mut dirty_frames) {
        fatal_qemu_test(format_args!(
            "elf dirty-frame release: {error:?}; allocator expected={baseline:?} actual={:?}",
            frames.stats()
        ));
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn release_dirty_elf_frames(
    frames: &mut FrameAllocator<512>,
    dirty_frames: &mut [Option<PhysFrame>; ELF_DIRTY_FRAME_COUNT],
) -> Option<FrameError> {
    let mut first_error = None;
    for slot in dirty_frames {
        if let Some(frame) = slot.take()
            && let Err(error) = frames.deallocate(frame)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
#[derive(Debug)]
enum ElfTestFailure {
    Entry {
        expected: u64,
        actual: u64,
    },
    Mapping {
        region: &'static str,
        address: u64,
        expected: PageFlags,
        actual: Result<(PhysAddr, PageFlags), VmError<IdentityFrameStoreError>>,
    },
    Read {
        region: &'static str,
        address: u64,
        error: VmError<IdentityFrameStoreError>,
    },
    Bytes {
        region: &'static str,
        address: u64,
        expected: [u8; 4],
        actual: [u8; 4],
    },
    NonZero {
        region: &'static str,
        address: u64,
        actual: u8,
    },
    Guard {
        address: u64,
        actual: Result<(PhysAddr, PageFlags), VmError<IdentityFrameStoreError>>,
    },
    UserSeparation {
        kernel: PageFlags,
        user: PageFlags,
    },
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
impl core::fmt::Display for ElfTestFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Entry { expected, actual } => {
                write!(
                    formatter,
                    "entry: expected={expected:#x}, actual={actual:#x}"
                )
            }
            Self::Mapping {
                region,
                address,
                expected,
                actual,
            } => write!(
                formatter,
                "{region} mapping @ {address:#x}: expected flags={expected:?}, actual={actual:?}"
            ),
            Self::Read {
                region,
                address,
                error,
            } => write!(
                formatter,
                "{region} bytes @ {address:#x}: translate/copy_out error={error:?}"
            ),
            Self::Bytes {
                region,
                address,
                expected,
                actual,
            } => write!(
                formatter,
                "{region} bytes @ {address:#x}: expected={expected:02x?}, actual={actual:02x?}"
            ),
            Self::NonZero {
                region,
                address,
                actual,
            } => write!(
                formatter,
                "{region} zero fill @ {address:#x}: expected=00, actual={actual:02x}"
            ),
            Self::Guard { address, actual } => write!(
                formatter,
                "guard @ {address:#x}: expected NotMapped, actual={actual:?}"
            ),
            Self::UserSeparation { kernel, user } => write!(
                formatter,
                "U-bit separation: expected kernel U=0/user U=1, actual kernel={kernel:?} user={user:?}"
            ),
        }
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn inspect_loaded_elf<const IMAGE_N: usize, const KERNEL_N: usize>(
    image: &LoadedImage<'_, IMAGE_N>,
    kernel_space: &AddressSpace<'_, KERNEL_N>,
    memory: &IdentityFrameStore,
) -> Result<(), ElfTestFailure> {
    const ENTRY: u64 = 0x0010_0000;
    const DATA: u64 = 0x0020_0000;

    if image.entry().as_u64() != ENTRY {
        return Err(ElfTestFailure::Entry {
            expected: ENTRY,
            actual: image.entry().as_u64(),
        });
    }

    let user_rx =
        PageFlags::new(true, false, true, true).expect("fixture user text permissions are valid");
    let user_rw =
        PageFlags::new(true, true, false, true).expect("fixture user data permissions are valid");
    let text_flags = expect_elf_mapping(image, memory, "text", ENTRY, user_rx)?;
    let mut text = [0u8; 4];
    copy_virtual(image, memory, "text", ENTRY, &mut text)?;
    if text != [0x13, 0x00, 0x00, 0x00] {
        return Err(ElfTestFailure::Bytes {
            region: "text",
            address: ENTRY,
            expected: [0x13, 0x00, 0x00, 0x00],
            actual: text,
        });
    }
    expect_zero_virtual(image, memory, "text padding", ENTRY + 4, PAGE_SIZE - 4)?;

    expect_elf_mapping(image, memory, "data", DATA, user_rw)?;
    let mut data = [0u8; 4];
    copy_virtual(image, memory, "data", DATA, &mut data)?;
    if data != *b"MCB1" {
        return Err(ElfTestFailure::Bytes {
            region: "data",
            address: DATA,
            expected: *b"MCB1",
            actual: data,
        });
    }
    expect_zero_virtual(image, memory, "data/BSS", DATA + 4, PAGE_SIZE - 4)?;

    expect_elf_mapping(image, memory, "stack", USER_STACK_BOTTOM, user_rw)?;
    expect_zero_virtual(
        image,
        memory,
        "stack first page",
        USER_STACK_BOTTOM,
        PAGE_SIZE,
    )?;

    let guard = VirtAddr::try_new(USER_GUARD_BOTTOM).expect("guard address is valid Sv39");
    let actual_guard = image.address_space().translate(memory, guard);
    if !matches!(actual_guard, Err(VmError::NotMapped)) {
        return Err(ElfTestFailure::Guard {
            address: USER_GUARD_BOTTOM,
            actual: actual_guard,
        });
    }

    let kernel_text = core::ptr::addr_of!(__text_start) as usize;
    let kernel_virtual =
        VirtAddr::try_new(kernel_text as u64).expect("linker text start is a valid Sv39 address");
    let kernel_actual = kernel_space.translate(memory, kernel_virtual);
    let kernel_flags = match kernel_actual {
        Ok((physical, flags))
            if physical.as_u64() == kernel_text as u64 && flags == PageFlags::supervisor_rx() =>
        {
            flags
        }
        actual => {
            return Err(ElfTestFailure::Mapping {
                region: "kernel text",
                address: kernel_text as u64,
                expected: PageFlags::supervisor_rx(),
                actual,
            });
        }
    };
    if kernel_flags.user() || !text_flags.user() {
        return Err(ElfTestFailure::UserSeparation {
            kernel: kernel_flags,
            user: text_flags,
        });
    }

    Ok(())
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn expect_elf_mapping<const N: usize>(
    image: &LoadedImage<'_, N>,
    memory: &IdentityFrameStore,
    region: &'static str,
    address: u64,
    expected: PageFlags,
) -> Result<PageFlags, ElfTestFailure> {
    let virtual_address = VirtAddr::try_new(address).expect("fixture addresses are valid Sv39");
    let actual = image.address_space().translate(memory, virtual_address);
    match actual {
        Ok((_physical, flags)) if flags == expected => Ok(flags),
        actual => Err(ElfTestFailure::Mapping {
            region,
            address,
            expected,
            actual,
        }),
    }
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn copy_virtual<const N: usize>(
    image: &LoadedImage<'_, N>,
    memory: &IdentityFrameStore,
    region: &'static str,
    address: u64,
    output: &mut [u8],
) -> Result<(), ElfTestFailure> {
    let virtual_address = VirtAddr::try_new(address).expect("fixture addresses are valid Sv39");
    let (physical, _flags) = image
        .address_space()
        .translate(memory, virtual_address)
        .map_err(|error| ElfTestFailure::Read {
            region,
            address,
            error,
        })?;
    let physical = physical.as_u64() as usize;
    let frame_start = physical & !(PAGE_SIZE - 1);
    let offset = physical & (PAGE_SIZE - 1);
    memory
        .copy_out(frame_start, offset, output)
        .map_err(|error| ElfTestFailure::Read {
            region,
            address,
            error: VmError::Store(error),
        })
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-elf"))]
fn expect_zero_virtual<const N: usize>(
    image: &LoadedImage<'_, N>,
    memory: &IdentityFrameStore,
    region: &'static str,
    start: u64,
    len: usize,
) -> Result<(), ElfTestFailure> {
    let mut checked = 0usize;
    let mut bytes = [0u8; 64];
    while checked < len {
        let address = start + checked as u64;
        let page_remaining = PAGE_SIZE - address as usize % PAGE_SIZE;
        let chunk_len = core::cmp::min(core::cmp::min(bytes.len(), len - checked), page_remaining);
        copy_virtual(image, memory, region, address, &mut bytes[..chunk_len])?;
        if let Some((index, actual)) = bytes[..chunk_len]
            .iter()
            .copied()
            .enumerate()
            .find(|(_, byte)| *byte != 0)
        {
            return Err(ElfTestFailure::NonZero {
                region,
                address: address + index as u64,
                actual,
            });
        }
        checked += chunk_len;
    }
    Ok(())
}

#[cfg(target_arch = "riscv64")]
unsafe extern "C" {
    // Safety: `user.S`が`sret`でU-modeへ降りる唯一の入口として公開するC ABI境界である。
    fn __run_user(
        context: *mut UserContext,
        user_satp: u64,
        kernel_satp: u64,
        kernel_stack_top: usize,
    ) -> RunExit;
    // Safety: `user.S`が公開するuser trap入口であり、4バイト境界にそろったsymbolである。
    fn __user_trap_entry();
}

#[cfg(target_arch = "riscv64")]
// `user.S`がシンボル名とC ABIを直接指定して呼ぶため、この名前とABIを変えてはならない。
#[unsafe(no_mangle)]
extern "C" fn rust_user_trap_handler(context: *mut UserContext) -> RunExit {
    // Safety: `user.S`がtrap frameとして組み立てた直後のkernel stack上の
    // 有効なcontextだけを渡す。
    unsafe { rust_user_trap_handler_impl(context) }
}

#[cfg(target_arch = "riscv64")]
unsafe fn rust_user_trap_handler_impl(context: *mut UserContext) -> RunExit {
    let scause = arch::riscv64::csr::read_scause();
    let stval = arch::riscv64::csr::read_stval();
    // Safety: 呼び出し側の契約により有効なcontextである。
    let context = unsafe { &mut *context };
    match minios_kernel::user::trap::handle_user_trap(context, scause, stval) {
        TrapAction::SystemCall => user_trap_system_call(context),
        TrapAction::Fatal { scause, stval } => user_trap_fatal(scause, stval),
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
    let space = unsafe { &*(USER_SYSCALL_PROBE_SPACE as *const AddressSpace<'_, 2688>) };
    let memory = unsafe { &*(USER_SYSCALL_PROBE_MEMORY as *const IdentityFrameStore) };
    let flow = dispatch_syscall(context, space, memory, &mut control::UartControlSink);
    match flow {
        SyscallFlow::Resume => RunExit::Resume,
        SyscallFlow::Exit(code) => {
            USER_EXIT_CODE.store(code as usize, Ordering::Relaxed);
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_EXIT, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        SyscallFlow::Fatal(()) => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_SINK_FAILURE, Ordering::Relaxed);
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
    // fatal trapの診断はcontrol modeではGuestError frameとしてhostへ届く。
    crate::console::emergency_print(format_args!(
        "MiniOS user trap: scause={scause:#018x} stval={stval:#018x}\r\n"
    ));
    USER_FATAL_SCAUSE.store(scause, Ordering::Relaxed);
    USER_FATAL_STVAL.store(stval, Ordering::Relaxed);
    USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_FATAL_TRAP, Ordering::Relaxed);
    RunExit::ReturnToKernel
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
    let space = unsafe { &*(USER_SYSCALL_PROBE_SPACE as *const AddressSpace<'_, 2688>) };
    let memory = unsafe { &*(USER_SYSCALL_PROBE_MEMORY as *const IdentityFrameStore) };
    let flow = dispatch_syscall(context, space, memory, &mut control::UartControlSink);
    match flow {
        SyscallFlow::Resume => RunExit::Resume,
        SyscallFlow::Exit(code) => {
            // このprobeのExit経路はsinkへ直接frameを1回載せる。
            use minios_kernel::user::syscall::ControlSink as _;
            let mut sink = control::UartControlSink;
            let _ = sink.frame(minios_abi::control::FrameKind::Exit, &code.to_le_bytes());
            RunExit::ReturnToKernel
        }
        SyscallFlow::Fatal(()) => {
            crate::console::emergency_print(format_args!(
                "[MINIOS_TEST] failed: user-syscall sink failure\r\n"
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
    // Safety: user-exit runnerが`__run_user`の直前に設定し、assemblyが
    // kernelへ戻るまで所有する単一hart静的参照である。
    let space = unsafe { &*(USER_SYSCALL_PROBE_SPACE as *const AddressSpace<'_, 2688>) };
    let memory = unsafe { &*(USER_SYSCALL_PROBE_MEMORY as *const IdentityFrameStore) };
    let flow = dispatch_syscall(context, space, memory, &mut control::UartControlSink);
    match flow {
        SyscallFlow::Resume => RunExit::Resume,
        SyscallFlow::Exit(code) => {
            USER_EXIT_CODE.store(code as usize, Ordering::Relaxed);
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_EXIT, Ordering::Relaxed);
            RunExit::ReturnToKernel
        }
        SyscallFlow::Fatal(()) => {
            USER_RUN_OUTCOME.store(USER_RUN_OUTCOME_SINK_FAILURE, Ordering::Relaxed);
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
    const SUPERVISOR_UART_PAGE: usize = 0x1000_0000;
    if scause != STORE_PAGE_FAULT || stval != SUPERVISOR_UART_PAGE {
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
fn run_user_mode_probe_test<const KERNEL_N: usize>(
    kernel_space: &AddressSpace<'_, KERNEL_N>,
    plan: &KernelMapPlan,
    frames: &mut FrameAllocator<512>,
    memory: &mut IdentityFrameStore,
) -> ! {
    // Safety: this feature path runs only on the single boot hart and obtains
    // the static ownership table once before loading the probe image.
    let storage_pointer = &raw mut USER_PROBE_ADDRESS_SPACE_STORAGE;
    let storage = unsafe { storage_pointer.as_mut() }
        .expect("a static user-probe storage pointer is never null");
    if !storage.is_empty() {
        fatal_qemu_test(format_args!(
            "user probe precondition: expected empty storage, actual len={}",
            storage.len()
        ));
    }

    #[cfg(feature = "qemu-test-user-entry")]
    let fixture = user_entry_ecall_elf();
    #[cfg(feature = "qemu-test-user-trap")]
    let fixture = user_trap_fault_elf();
    #[cfg(feature = "qemu-test-user-syscall")]
    let fixture = user_syscall_probe_elf();
    let image =
        match load_image_with_kernel_mappings(&fixture, frames, memory, storage, plan.mappings()) {
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
fn run_user_exit_test<const KERNEL_N: usize>(
    kernel_space: &AddressSpace<'_, KERNEL_N>,
    plan: &KernelMapPlan,
    frames: &mut FrameAllocator<512>,
    memory: &mut IdentityFrameStore,
) {
    let before = frames.stats();
    // Safety: このfeatureは単一boot hartでだけ実行し、このstorageを一度だけ
    // 取得する。runを破棄するまで別の参照を作らない。
    let storage_pointer = &raw mut USER_PROBE_ADDRESS_SPACE_STORAGE;
    let storage = unsafe { storage_pointer.as_mut() }
        .expect("a static user-exit storage pointer is never null");
    if !storage.is_empty() {
        fatal_qemu_test(format_args!(
            "user-exit precondition: expected empty storage, actual len={}",
            storage.len()
        ));
    }

    let fixture = user_exit_probe_elf();
    let image =
        match load_image_with_kernel_mappings(&fixture, frames, memory, storage, plan.mappings()) {
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
            USER_RUN_OUTCOME_FATAL_TRAP | USER_RUN_OUTCOME_SINK_FAILURE => RunOutcome::Fatal,
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
    let storage_len = storage.len();
    if after != before || storage_len != 0 {
        fatal_qemu_test(format_args!(
            "user-exit recovery: allocator expected={before:?} actual={after:?}; storage expected len=0 actual len={storage_len}"
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

#[cfg(target_arch = "riscv64")]
fn fatal_payload_error(arguments: core::fmt::Arguments<'_>) -> ! {
    // payload pathの失敗はhost (minictr) が即座に検出できるよう異常shutdownする。
    crate::console::emergency_print(arguments);
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

/// production payload path: Ready frameを送り、予約窓のELFをuser実行し、
/// Exit後のresource回収を検証してからshutdownする。
#[cfg(target_arch = "riscv64")]
fn run_boot_payload<const KERNEL_N: usize>(
    kernel_space: &AddressSpace<'_, KERNEL_N>,
    plan: &KernelMapPlan,
    frames: &mut FrameAllocator<512>,
    memory: &mut IdentityFrameStore,
    payload: BootPayload<'static>,
) -> ! {
    let before = frames.stats();
    // Safety: このpathは単一boot hartでだけ実行し、このstorageを一度だけ取得する。
    let storage_pointer = &raw mut PAYLOAD_ADDRESS_SPACE_STORAGE;
    let storage = unsafe { storage_pointer.as_mut() }
        .expect("a static payload storage pointer is never null");
    if !storage.is_empty() {
        fatal_payload_error(format_args!(
            "MiniOS payload: storage precondition failed, len={}\r\n",
            storage.len()
        ));
    }

    let image = match load_image_with_kernel_mappings(
        payload.elf(),
        frames,
        memory,
        storage,
        plan.mappings(),
    ) {
        Ok(image) => image,
        Err(error) => fatal_payload_error(format_args!("MiniOS payload: load, {error:?}\r\n")),
    };
    // imageはUserRunへmoveされるため、entryとstack topは先にcontextへ固定する。
    let mut context = UserContext::new(image.entry(), image.user_stack_top());
    let kernel_root = PhysPageNum::from_start(kernel_space.root().as_u64())
        .expect("kernel root page number is valid");
    let mut run = match UserRun::new(image, frames, memory, kernel_root) {
        Ok(run) => run,
        Err(error) => fatal_payload_error(format_args!("MiniOS payload: run build, {error:?}\r\n")),
    };

    // Safety: pointer値を保存するだけでここでは解参照しない。handlerだけが
    // assemblyの実行窓で読み、kernelへ戻った直後に0へ戻す。
    unsafe {
        USER_SYSCALL_PROBE_SPACE = run.address_space() as *const _ as usize;
        USER_SYSCALL_PROBE_MEMORY = run.memory() as *const IdentityFrameStore as usize;
    }

    // payloadはtimer tickに依存せず、Supervisor timer割り込みはuser trapの
    // 分類上Fatalなので、実行窓だけSTIEを無効化する。
    const SIE_STIE: usize = 1 << 5;
    // Safety: S-modeで`sie`と`stvec`を書く。
    unsafe {
        let sie = arch::riscv64::csr::read_sie();
        arch::riscv64::csr::write_sie(sie & !SIE_STIE);
        arch::riscv64::csr::write_stvec(__user_trap_entry as *const () as usize);
    }

    // Ready frameを最後のplain text出力の後に送り、以降のUARTをcontrol frame
    // へ限定する。
    control::send_ready();

    let completion = {
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
                _ => RunOutcome::Fatal,
            }
        });
        // Safety: handlerが今後走らないkernel側へ戻ったため、danglingになり得る
        // pointer値を解放前に無効化する。
        unsafe {
            USER_SYSCALL_PROBE_SPACE = 0;
            USER_SYSCALL_PROBE_MEMORY = 0;
        }
        completion
    };
    let completion = match completion {
        Ok(completion) => completion,
        Err(error) => fatal_payload_error(format_args!("MiniOS payload: finish, {error:?}\r\n")),
    };

    let after = frames.stats();
    let storage_len = storage.len();
    if after != before || storage_len != 0 {
        fatal_payload_error(format_args!(
            "MiniOS payload: recovery, allocator expected={before:?} actual={after:?}; storage len={storage_len}\r\n"
        ));
    }

    match completion {
        RunCompletion::Exit(code) => {
            // control mode中の唯一のplain text出力経路であり、Diagnostic frame
            // としてhostへ届くresource cleanup markerである。
            crate::println!("\r\nMiniOS payload: ok code={code}");
            successful_payload_shutdown()
        }
        RunCompletion::Fatal => fatal_payload_error(format_args!(
            "MiniOS payload: fatal, scause={:#018x} stval={:#018x}\r\n",
            USER_FATAL_SCAUSE.load(Ordering::Relaxed),
            USER_FATAL_STVAL.load(Ordering::Relaxed)
        )),
    }
}

#[cfg(target_arch = "riscv64")]
fn successful_payload_shutdown() -> ! {
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::NoReason,
    )
}

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-vm",
        feature = "qemu-test-elf",
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit"
    )
))]
fn successful_qemu_test_shutdown() -> ! {
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::NoReason,
    )
}

#[cfg(all(
    target_arch = "riscv64",
    any(
        feature = "qemu-test-vm",
        feature = "qemu-test-elf",
        feature = "qemu-test-user-entry",
        feature = "qemu-test-user-trap",
        feature = "qemu-test-user-syscall",
        feature = "qemu-test-user-exit"
    )
))]
fn fatal_qemu_test(arguments: core::fmt::Arguments<'_>) -> ! {
    crate::console::emergency_print(format_args!("[MINIOS_TEST] failed: {arguments}\r\n"));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-memory"))]
fn run_memory_test(frames: &mut FrameAllocator<512>) -> ! {
    let first = match frames.allocate() {
        Some(frame) => frame,
        None => fatal_memory_test(),
    };
    let first_start = first.start();
    let second = match frames.allocate() {
        Some(frame) => frame,
        None => fatal_memory_test(),
    };
    // アロケーターは4 KiB単位の物理フレームを返すため、二つのアドレスが異なり、境界にそろっていることをゲスト上で確認する。
    if first == second || first.start() % PAGE_SIZE != 0 || second.start() % PAGE_SIZE != 0 {
        fatal_memory_test();
    }
    if frames.deallocate(first).is_err() {
        fatal_memory_test();
    }
    let reused = match frames.allocate() {
        Some(frame) => frame,
        None => fatal_memory_test(),
    };
    if reused.start() != first_start {
        fatal_memory_test();
    }
    crate::println!("[MINIOS_TEST] memory: ok");
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::NoReason,
    )
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-memory"))]
fn fatal_memory_test() -> ! {
    crate::console::emergency_print(format_args!("MiniOS memory test failed\r\n"));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(target_arch = "riscv64")]
fn fatal_timer_error(operation: &str, error: minios_kernel::sbi::SbiError) -> ! {
    // タイマを再設定できなければ次の割り込み時刻を保証できず回復規約もないため、
    // ロック不要の診断経路で SBI エラーを残し、失敗理由で停止する。
    crate::console::emergency_print(format_args!(
        "MiniOS timer: {operation} failed with SBI error {}\r\n",
        error.0
    ));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(target_arch = "riscv64")]
fn fatal_memory_error(error: FrameError) -> ! {
    // リンカーの範囲とビットマップ容量が一致しなければ、安全な未使用ページ集合を作れないため、診断を出して異常終了する。
    crate::console::emergency_print(format_args!("MiniOS memory: {error:?}\r\n"));
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}

#[cfg(target_arch = "riscv32")]
// `entry.S`はスタックとBSSを整えてからこのABI境界を呼ぶ。
// NEORV32は単一ハートのMモード起動であり、OpenSBIやDTBは存在しない。
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main32() -> ! {
    BOOT_HART_ID.store(0, Ordering::Relaxed);
    console::init();
    crate::println!("MiniOS/RV32 booting...");
    crate::println!("hart id: 0");
    shell::run32(0)
}

#[cfg(target_arch = "riscv32")]
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let hart_id = BOOT_HART_ID.load(Ordering::Relaxed);
    match (info.location(), hart_id != UNKNOWN_HART_ID) {
        (Some(location), true) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nfile: {}\r\nline: {}\r\nhart id: {}\r\n",
            info.message(),
            location.file(),
            location.line(),
            hart_id
        )),
        (Some(location), false) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nfile: {}\r\nline: {}\r\n",
            info.message(),
            location.file(),
            location.line()
        )),
        (None, true) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nhart id: {}\r\n",
            info.message(),
            hart_id
        )),
        (None, false) => {
            crate::console::emergency_print(format_args!("MiniOS panic: {}\r\n", info.message()))
        }
    }
    // NEORV32にSBIはないため、割り込み待ちで停止する。
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

#[cfg(target_arch = "riscv64")]
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let hart_id = BOOT_HART_ID.load(Ordering::Relaxed);
    match (info.location(), hart_id != UNKNOWN_HART_ID) {
        (Some(location), true) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nfile: {}\r\nline: {}\r\nhart id: {}\r\n",
            info.message(),
            location.file(),
            location.line(),
            hart_id
        )),
        (Some(location), false) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nfile: {}\r\nline: {}\r\n",
            info.message(),
            location.file(),
            location.line()
        )),
        (None, true) => crate::console::emergency_print(format_args!(
            "MiniOS panic: {}\r\nhart id: {}\r\n",
            info.message(),
            hart_id
        )),
        (None, false) => {
            crate::console::emergency_print(format_args!("MiniOS panic: {}\r\n", info.message()))
        }
    }
    arch::riscv64::sbi::system_reset(
        arch::riscv64::sbi::ResetType::Shutdown,
        arch::riscv64::sbi::ResetReason::SystemFailure,
    )
}
