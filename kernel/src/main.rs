#![no_std]
#![no_main]

#[cfg(target_arch = "riscv64")]
mod arch;
#[cfg(target_arch = "riscv64")]
mod console;
#[cfg(target_arch = "riscv64")]
mod drivers;
#[cfg(target_arch = "riscv64")]
mod time;

#[cfg(target_arch = "riscv64")]
use core::panic::PanicInfo;
#[cfg(target_arch = "riscv64")]
use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(target_arch = "riscv64")]
use minios_kernel::memory::frame::{FrameAllocator, FrameError, PAGE_SIZE};

#[cfg(target_arch = "riscv64")]
const UNKNOWN_HART_ID: usize = usize::MAX;
#[cfg(target_arch = "riscv64")]
static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(UNKNOWN_HART_ID);

#[cfg(target_arch = "riscv64")]
// QEMU virt を 128 MiB で起動するため、RAM の上端 0x8800_0000 を allocator の固定境界とする。
const PHYSICAL_MEMORY_END: usize = 0x8800_0000;

#[cfg(target_arch = "riscv64")]
unsafe extern "C" {
    // linker.ld が kernel image の直後へ置く境界であり、OpenSBI とカーネル自身を割り当て対象から除外する。
    static __kernel_end: u8;
}

#[cfg(target_arch = "riscv64")]
// リンカの entry.S は `kernel_main` という外部 C ABI シンボルを呼ぶ。OpenSBI の
// a0/a1 を hart_id/dtb としてそのまま受け取るため、この名前と ABI を変えてはならない。
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(hart_id: usize, dtb: usize) -> ! {
    // パニック診断が起動 hart を識別できるよう、他の初期化より前に公開する。
    BOOT_HART_ID.store(hart_id, Ordering::Relaxed);
    // DTB は後のハードウェア検出で使うまで保持する OpenSBI の ABI 引数である。
    let _ = dtb;
    arch::riscv64::trap::init();
    crate::println!("[ok] traps");
    if let Err(error) = time::init() {
        fatal_timer_error("initial schedule", error);
    }
    crate::println!("[ok] timer");

    let frames = match FrameAllocator::<512>::new(kernel_memory_start(), PHYSICAL_MEMORY_END) {
        Ok(frames) => frames,
        Err(error) => fatal_memory_error(error),
    };
    crate::println!("[ok] memory");

    #[cfg(feature = "qemu-test-memory")]
    {
        let mut frames = frames;
        run_memory_test(&mut frames);
    }

    #[cfg(not(feature = "qemu-test-memory"))]
    let _ = frames;

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
        // Safety: ebreak はメモリやスタックを変更せず同期 breakpoint 例外を発生させる。
        // trap::init 後であり、ハンドラが成功 reset することがこのテスト ABI である。
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

    arch::riscv64::sbi::wait_for_interrupt()
}

#[cfg(target_arch = "riscv64")]
fn kernel_memory_start() -> usize {
    // Safety: __kernel_end は linker.ld が実在する image 境界として定義し、そのアドレスを読むだけでメモリ内容にはアクセスしない。
    let kernel_end = unsafe { core::ptr::addr_of!(__kernel_end) as usize };
    // linker.ld はこのシンボルを 4 KiB 整列で出力するが、将来の linker 変更後も先頭ページを上へ丸めて image を保護する。
    align_up_to_page(kernel_end)
}

#[cfg(target_arch = "riscv64")]
fn align_up_to_page(address: usize) -> usize {
    // linker image は 0x8800_0000 より十分低い物理アドレスにあるため、PAGE_SIZE - 1 の加算は usize をオーバーフローしない。
    (address + (PAGE_SIZE - 1)) & !(PAGE_SIZE - 1)
}

#[cfg(all(target_arch = "riscv64", feature = "qemu-test-memory"))]
fn run_memory_test(frames: &mut FrameAllocator<512>) -> ! {
    let first = match frames.allocate() {
        Some(frame) => frame,
        None => fatal_memory_test(),
    };
    let second = match frames.allocate() {
        Some(frame) => frame,
        None => fatal_memory_test(),
    };
    // allocator の公開契約は 4 KiB ごとの物理 frame なので、返却アドレスが区別され整列していることを実機範囲で確認する。
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
    if reused != first {
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
    // linker 範囲と bitmap 容量の不整合では安全な free page 集合を作れないため、診断して失敗理由で停止する。
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
