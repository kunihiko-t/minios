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
    PHYSICAL_MEMORY_END,
    frame::{FrameAllocator, FrameError, PAGE_SIZE},
};

#[cfg(target_arch = "riscv64")]
const UNKNOWN_HART_ID: usize = usize::MAX;
#[cfg(target_arch = "riscv64")]
static BOOT_HART_ID: AtomicUsize = AtomicUsize::new(UNKNOWN_HART_ID);

#[cfg(target_arch = "riscv64")]
// Safety: `linker.ld`がRISC-Vカーネルイメージの末尾に必ず定義するC ABIシンボルである。
// Rust側は`addr_of!`でアドレスを作るだけで、外部staticの内容を読み書きしない。
unsafe extern "C" {
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
    if let Err(error) = time::init() {
        fatal_timer_error("initial schedule", error);
    }
    crate::println!("[ok] timer");

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
