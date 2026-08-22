use core::sync::atomic::{AtomicU64, Ordering};

pub const TIMEBASE_HZ: u64 = 10_000_000;
pub const TICKS_PER_SECOND: u64 = 100;
pub const CYCLES_PER_TICK: u64 = 100_000;

// QEMU virt の timebase と選んだ tick 周波数が固定周期と一致することをコンパイル時に保つ。
const _: () = assert!(TIMEBASE_HZ / TICKS_PER_SECOND == CYCLES_PER_TICK);

static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn ticks() -> u64 {
    // tick 値は他状態の公開を担わない単独の統計値なので Relaxed load で十分である。
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_millis() -> u64 {
    ticks_to_millis(ticks())
}

pub fn ticks_to_millis(ticks: u64) -> u64 {
    ticks.saturating_mul(1_000) / TICKS_PER_SECOND
}

#[cfg(target_arch = "riscv64")]
pub fn init() -> Result<(), minios_kernel::sbi::SbiError> {
    schedule_next()?;

    let sie = crate::arch::riscv64::csr::read_sie();
    // Safety: trap::init 後で code 5 のハンドラと tick 状態が初期化済みであり、
    // 既存の WARL bit を保存したまま STIE (bit 5) だけを有効にする。
    unsafe { crate::arch::riscv64::csr::write_sie(sie | (1 << 5)) };

    let sstatus = crate::arch::riscv64::csr::read_sstatus();
    // Safety: STIE と最初の絶対 deadline を先に設定済みなので、既存 bit を保存しつつ
    // 最後に global SIE (bit 1) を有効化しても未処理の割り込みへは入らない。
    unsafe { crate::arch::riscv64::csr::write_sstatus(sstatus | (1 << 1)) };
    Ok(())
}

#[cfg(target_arch = "riscv64")]
pub fn handle_interrupt() -> Result<(), minios_kernel::sbi::SbiError> {
    // 単一 hart の handler だけが書き込み、読者は経過値だけを観測するため Relaxed で十分である。
    TICKS.fetch_add(1, Ordering::Relaxed);
    schedule_next()
}

#[cfg(target_arch = "riscv64")]
fn schedule_next() -> Result<(), minios_kernel::sbi::SbiError> {
    let deadline = crate::arch::riscv64::csr::read_time().wrapping_add(CYCLES_PER_TICK);
    crate::arch::riscv64::sbi::set_timer(deadline).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::ticks_to_millis;

    #[test]
    fn converts_ticks_to_milliseconds() {
        assert_eq!(ticks_to_millis(0), 0);
        assert_eq!(ticks_to_millis(1), 10);
        assert_eq!(ticks_to_millis(250), 2_500);
    }
}
