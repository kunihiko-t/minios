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
const UNKNOWN_HART_ID: usize = usize::MAX;
#[cfg(target_arch = "riscv64")]
static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(UNKNOWN_HART_ID);

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
    if let Err(error) = time::init() {
        fatal_timer_error("initial schedule", error);
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
