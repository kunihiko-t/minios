use core::arch::asm;

// ターゲットに依存しないSBI戻り値の型を、この`ecall`境界からも公開する。
pub use minios_kernel::sbi::{SbiError, SbiRet};

// SBI仕様がTIME拡張に割り当てた拡張IDと、`set_timer`の関数IDである。
const SBI_EXT_TIME: usize = 0x5449_4d45;
const SBI_TIME_SET_TIMER: usize = 0;
// SBI仕様がSRST拡張に割り当てた拡張IDであり、OpenSBIの実装と一致させる。
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
    // SBI SRST仕様ではシステム障害の理由IDが1であり、パニックと予期しないトラップを正常終了から区別できる。
    SystemFailure = 1,
}

pub fn set_timer(deadline: u64) -> Result<usize, SbiError> {
    // RV64の`a0`は64ビット幅なので、絶対時刻を分割せずSBI TIMEへ渡せる。
    sbi_call(deadline as usize, 0, 0, SBI_TIME_SET_TIMER, SBI_EXT_TIME).into_result()
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
    // Safety: SBI v0.2以降は`a0..a2/a6/a7`の入出力ABIを定め、`ecall`後の`a0/a1`をエラーと値として返す。
    // この命令列はスタックを読み書きしないため、`nostack`を指定できる。
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
        // Safety: RISC-Vの`wfi`は現在の特権モードで待機するだけで、メモリーを参照しない。
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

fn wait_with_interrupts_disabled() -> ! {
    // Safety: S-modeの`sstatus.SIE`（ビット1）を消し、初期化されていない割り込み処理へ入ることを防ぐ。
    // SBIリセットが戻った異常経路だけから呼び出す。
    unsafe { asm!("csrci sstatus, 2", options(nomem, nostack)) };
    wait_for_interrupt()
}
