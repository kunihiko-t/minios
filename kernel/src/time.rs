use core::sync::atomic::{AtomicU64, Ordering};

pub const TIMEBASE_HZ: u64 = 10_000_000;
pub const TICKS_PER_SECOND: u64 = 100;
pub const CYCLES_PER_TICK: u64 = 100_000;

// QEMU virtのタイムベースと選んだティック周波数が固定周期と一致することを、コンパイル時に確かめる。
const _: () = assert!(TIMEBASE_HZ / TICKS_PER_SECOND == CYCLES_PER_TICK);

static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn ticks() -> u64 {
    // ティックはほかの状態の公開を担わない単独の統計値なので、`Relaxed`な読み取りで十分である。
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
    // Safety: `trap::init`後であり、原因コード5のハンドラーとティックの状態は初期化済みである。
    // 既存のWARLビットを保ったまま、STIE（ビット5）だけを有効にする。
    unsafe { crate::arch::riscv64::csr::write_sie(sie | (1 << 5)) };

    let sstatus = crate::arch::riscv64::csr::read_sstatus();
    // Safety: STIEと最初の絶対デッドラインを先に設定している。
    // 既存ビットを保ったまま、最後に全体のSIE（ビット1）を有効にしても、未設定の割り込み処理へは入らない。
    unsafe { crate::arch::riscv64::csr::write_sstatus(sstatus | (1 << 1)) };
    Ok(())
}

#[cfg(target_arch = "riscv64")]
pub fn handle_interrupt() -> Result<(), minios_kernel::sbi::SbiError> {
    // シングルハートのハンドラーだけが書き込み、読み手は経過値だけを観測するため、`Relaxed`で十分である。
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
