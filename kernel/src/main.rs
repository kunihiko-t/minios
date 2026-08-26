#![no_std]
#![no_main]

#[cfg(target_arch = "riscv64")]
mod arch;
#[cfg(target_arch = "riscv64")]
mod console;
#[cfg(target_arch = "riscv64")]
mod drivers;
#[cfg(target_arch = "riscv64")]
mod shell;
#[cfg(target_arch = "riscv64")]
mod time;

#[cfg(target_arch = "riscv64")]
use core::panic::PanicInfo;
#[cfg(target_arch = "riscv64")]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_arch = "riscv64")]
use minios_kernel::memory::{
    KernelSections, PHYSICAL_MEMORY_END,
    frame::{FrameAllocator, FrameError, PAGE_SIZE},
};
#[cfg(target_arch = "riscv64")]
use minios_kernel::vm::{
    AddressSpaceBuilder, AddressSpaceStorage, IdentityFrameStore, KernelMapPlan, PhysPageNum,
};

#[cfg(target_arch = "riscv64")]
const UNKNOWN_HART_ID: usize = usize::MAX;
#[cfg(target_arch = "riscv64")]
static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(UNKNOWN_HART_ID);

#[cfg(target_arch = "riscv64")]
static mut KERNEL_ADDRESS_SPACE_STORAGE: AddressSpaceStorage<2688> = AddressSpaceStorage::new();

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
    let plan = match KernelMapPlan::new(&sections, managed_memory_start, PHYSICAL_MEMORY_END) {
        Ok(plan) => plan,
        Err(error) => panic!("invalid kernel mapping plan: {error:?}"),
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

    #[cfg(feature = "qemu-test-memory")]
    {
        run_memory_test(&mut frames);
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
