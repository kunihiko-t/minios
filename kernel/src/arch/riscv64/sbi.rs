use core::arch::asm;

// Task 2 で分離したターゲット非依存型をこの ecall 境界から引き続き公開する。
#[allow(unused_imports)]
pub use minios_kernel::sbi::{SbiError, SbiRet};

// SBI 仕様で SRST 拡張に固定された extension ID であり、OpenSBI と一致させる。
const SBI_EXT_SYSTEM_RESET: usize = 0x5352_5354;
const SBI_SYSTEM_RESET: usize = 0;

#[repr(usize)]
#[derive(Debug, Clone, Copy)]
pub enum ResetType {
    Shutdown = 0,
}

#[repr(usize)]
#[derive(Debug, Clone, Copy)]
pub enum ResetReason {
    NoReason = 0,
    SystemFailure = 1,
}

pub fn system_reset(reset_type: ResetType, reason: ResetReason) -> ! {
    let result = sbi_call(
        reset_type as usize,
        reason as usize,
        0,
        SBI_SYSTEM_RESET,
        SBI_EXT_SYSTEM_RESET,
    );
    if let Err(error) = result.into_result() {
        crate::console::emergency_sbi_error(error.0);
    }
    wait_with_interrupts_disabled()
}

fn sbi_call(
    argument0: usize,
    argument1: usize,
    argument2: usize,
    function: usize,
    extension: usize,
) -> SbiRet {
    let error: isize;
    let value: usize;
    // Safety: SBI v0.2+ は a0..a2/a6/a7 の入出力 ABI を規定し、ecall 後の a0/a1 を
    // error/value として返す。nostack はこの命令列がスタックを読書きしない条件である。
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") argument0 => error,
            inlateout("a1") argument1 => value,
            in("a2") argument2,
            in("a6") function,
            in("a7") extension,
            options(nostack),
        );
    }
    SbiRet { error, value }
}

pub fn wait_for_interrupt() -> ! {
    loop {
        // Safety: RISC-V の wfi は現在の特権モードで待機するだけで、メモリ参照をしない。
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

fn wait_with_interrupts_disabled() -> ! {
    // Safety: S-mode の sstatus.SIE (bit 1) をクリアし、未初期化の割り込み処理へ
    // 入らないようにする。SBI reset が返った異常経路だけから呼び出す。
    unsafe { asm!("csrci sstatus, 2", options(nomem, nostack)) };
    wait_for_interrupt()
}
