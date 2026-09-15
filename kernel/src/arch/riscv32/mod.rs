#[cfg(target_arch = "riscv32")]
// Safety: `entry.S`はNEORV32のMモード起動(`pc=0`、スタック未設定、BSS未初期化)を前提にする。
// Rustを呼ぶ前にスタック・BSS・`mtvec`の不変条件を確立するため、異なるアーキテクチャーへは組み込まない。
core::arch::global_asm!(include_str!("entry.S"));

// NEORV32のデータシートが規定する96 MHz基準クロックである。
// UARTのボーレート計算、SDの遅延、シェルの`uptime`換算がこの一つの値を共有する。
#[cfg(target_arch = "riscv32")]
pub const SYSTEM_CLOCK_HZ: u32 = 96_000_000;

/// 64ビットの`cycle`カウンターを読む。
/// RV32では`cycle`と`cycleh`へ分かれるため、読み取り中に上位が進んだら読み直して一貫した値を得る。
#[cfg(target_arch = "riscv32")]
pub fn cycles() -> u64 {
    loop {
        let (hi_before, lo, hi_after): (u32, u32, u32);
        // Safety: `rdcycle`/`rdcycleh`はNEORV32 RV32IM実行modeで利用でき、
        // memoryとstackに触れない。
        unsafe {
            core::arch::asm!("rdcycleh {0}", out(reg) hi_before, options(nomem, nostack));
            core::arch::asm!("rdcycle {0}", out(reg) lo, options(nomem, nostack));
            core::arch::asm!("rdcycleh {0}", out(reg) hi_after, options(nomem, nostack));
        }
        if hi_before == hi_after {
            return ((hi_before as u64) << 32) | lo as u64;
        }
    }
}

/// 割り込み待ちでCPUを停止する。
/// NEORV32には電源切断機構がなく、有効な割り込みを持たないシェルの`shutdown`が
/// 事実上の停止として使う。
#[cfg(target_arch = "riscv32")]
pub fn wfi() {
    // Safety: `wfi`はM-modeで常に有効であり、memoryとstackに触れない。
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
}
