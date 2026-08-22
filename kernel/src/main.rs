#![no_std]
#![no_main]

#[cfg(target_arch = "riscv64")]
mod arch;
#[cfg(target_arch = "riscv64")]
mod console;
#[cfg(target_arch = "riscv64")]
mod drivers;

#[cfg(target_arch = "riscv64")]
use core::panic::PanicInfo;

#[cfg(target_arch = "riscv64")]
// リンカの entry.S は `kernel_main` という外部 C ABI シンボルを呼ぶ。OpenSBI の
// a0/a1 を hart_id/dtb としてそのまま受け取るため、この名前と ABI を変えてはならない。
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(hart_id: usize, dtb: usize) -> ! {
    // DTB は後のハードウェア検出で使うまで保持する OpenSBI の ABI 引数である。
    let _ = dtb;
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

    #[cfg(not(feature = "qemu-test-boot"))]
    arch::riscv64::sbi::wait_for_interrupt()
}

#[cfg(target_arch = "riscv64")]
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    crate::println!("MiniOS panic");
    arch::riscv64::sbi::wait_for_interrupt()
}
