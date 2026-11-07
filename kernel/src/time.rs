#[cfg(target_arch = "riscv64")]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::{AtomicU64, Ordering};

// QEMU `virt`のtimebase-frequencyの参照値。実行時はFDTの`/cpus`から得た
// 値を`init`へ渡して使う。
pub const TIMEBASE_HZ: u64 = 10_000_000;
pub const TICKS_PER_SECOND: u64 = 100;
pub const CYCLES_PER_TICK: u64 = 100_000;

// 参照値のtimebaseと選んだティック周波数が固定周期と一致することを、コンパイル時に確かめる。
const _: () = assert!(TIMEBASE_HZ / TICKS_PER_SECOND == CYCLES_PER_TICK);

// FDTから発見したtimebase。`init`が一度だけ書き、タイマー割り込みの
// デッドライン計算だけが読む。
#[cfg(target_arch = "riscv64")]
static TIMEBASE: AtomicU64 = AtomicU64::new(TIMEBASE_HZ);

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

/// `sleep`の`millis`を起床tickへ変換する。tick数は切り上げ、さらに1を
/// 足す。現在のtickは既に途中まで経過しているため、`now + 切り上げ値`
/// だと実時間で最大1 tick早く起きてしまうからである。
pub fn sleep_deadline(now: u64, millis: usize) -> u64 {
    let ticks = (millis as u64)
        .saturating_mul(TICKS_PER_SECOND)
        .div_ceil(1_000);
    now.saturating_add(ticks).saturating_add(1)
}

// 実時刻の起点。起動時にRTCから読んだUnix時刻（nanosecond）と、その瞬間の
// `time` CSRを組で持つ。RTCのregisterを毎回読むにはMMIO pageをkernelと全user
// address spaceへmapし続ける必要があるため、起動時に1回だけ読み、以後は
// `time` CSRの経過分を足して進める。RTCを持たないmachineでは
// `REALTIME_KNOWN`がfalseのままで、実時刻は返さない。
#[cfg(target_arch = "riscv64")]
static REALTIME_ORIGIN_NANOS: AtomicU64 = AtomicU64::new(0);
#[cfg(target_arch = "riscv64")]
static REALTIME_ORIGIN_TIME: AtomicU64 = AtomicU64::new(0);
#[cfg(target_arch = "riscv64")]
static REALTIME_KNOWN: AtomicBool = AtomicBool::new(false);

/// 起動時にRTCから読んだUnix時刻`unix_nanos`と、読んだ瞬間の`time` CSR
/// `time_csr`を実時刻の起点として記録する。`kernel_main`が一度だけ呼ぶ。
#[cfg(target_arch = "riscv64")]
pub fn set_realtime_origin(unix_nanos: u64, time_csr: u64) {
    REALTIME_ORIGIN_NANOS.store(unix_nanos, Ordering::Relaxed);
    REALTIME_ORIGIN_TIME.store(time_csr, Ordering::Relaxed);
    // 起点の2値を書いてから公開する。読み手は`Acquire`でこれを見る。
    REALTIME_KNOWN.store(true, Ordering::Release);
}

/// 現在のUnix時刻（millisecond）。RTCの無いmachineでは`None`。
#[cfg(target_arch = "riscv64")]
pub fn realtime_millis() -> Option<u64> {
    if !REALTIME_KNOWN.load(Ordering::Acquire) {
        return None;
    }
    Some(realtime_millis_at(
        REALTIME_ORIGIN_NANOS.load(Ordering::Relaxed),
        REALTIME_ORIGIN_TIME.load(Ordering::Relaxed),
        crate::arch::riscv64::csr::read_time(),
        TIMEBASE.load(Ordering::Relaxed),
    ))
}

/// 起点`(origin_nanos, origin_time)`から`time` CSRが`now`まで進んだときの
/// Unix時刻（millisecond）。経過cycleは`timebase_hz`で割ってmillisecondへ
/// 直す。途中の積は64 bitを超えうるため128 bitで計算する。
pub fn realtime_millis_at(origin_nanos: u64, origin_time: u64, now: u64, timebase_hz: u64) -> u64 {
    let elapsed = u128::from(now.wrapping_sub(origin_time));
    let elapsed_nanos = elapsed * 1_000_000_000 / u128::from(timebase_hz.max(1));
    let nanos = u128::from(origin_nanos) + elapsed_nanos;
    u64::try_from(nanos / 1_000_000).unwrap_or(u64::MAX)
}

/// 現在processをdispatchした時刻（`time` CSR）。run loopが`__run_user`の
/// 直前に`start_quantum`で書き、U-mode中のtimer割り込みが`quantum_expired`で
/// 読む。同時にdispatchされるprocessは1つだけである。
#[cfg(target_arch = "riscv64")]
static QUANTUM_START: AtomicU64 = AtomicU64::new(0);

/// 今からdispatchするprocessの持ち時間を始める。
#[cfg(target_arch = "riscv64")]
pub fn start_quantum() {
    QUANTUM_START.store(crate::arch::riscv64::csr::read_time(), Ordering::Relaxed);
}

/// dispatchしてから1 tick分の時間が経ったか。tickは時刻を数えるために毎回
/// 処理するが、preemptはこれが真のときだけにする。前のprocessがkernel内で
/// 長く過ごす間に期限を過ぎたtickは、次のprocessのdispatch直後に届くため、
/// tickのたびにpreemptすると次のprocessは何もしないうちに順番を失う。
#[cfg(target_arch = "riscv64")]
pub fn quantum_expired() -> bool {
    quantum_elapsed(
        QUANTUM_START.load(Ordering::Relaxed),
        crate::arch::riscv64::csr::read_time(),
        TIMEBASE.load(Ordering::Relaxed) / TICKS_PER_SECOND,
    )
}

/// `start`から`now`までに`cycles_per_tick`以上経ったか。`time` CSRは
/// 単調増加なので、wrapしても差は正しく求まる。
pub fn quantum_elapsed(start: u64, now: u64, cycles_per_tick: u64) -> bool {
    now.wrapping_sub(start) >= cycles_per_tick
}

/// timer割り込みがpendingか（`sip.STIP`）。run loopのidle待ちは
/// S-mode割り込みを遮断したまま回るため、これをpollしてtickを進める。
#[cfg(target_arch = "riscv64")]
pub fn interrupt_pending() -> bool {
    crate::arch::riscv64::csr::read_sip() & (1 << 5) != 0
}

/// `timebase_hz`はFDTの`/cpus` `timebase-frequency`が示す`time` CSRの周波数。
/// ティック周期は`timebase_hz / TICKS_PER_SECOND`サイクルへ丸める。
#[cfg(target_arch = "riscv64")]
pub fn init(timebase_hz: u64) -> Result<(), minios_kernel::sbi::SbiError> {
    assert!(
        timebase_hz >= TICKS_PER_SECOND,
        "timebase too slow for 100 Hz ticks"
    );
    TIMEBASE.store(timebase_hz, Ordering::Relaxed);
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
    let cycles_per_tick = TIMEBASE.load(Ordering::Relaxed) / TICKS_PER_SECOND;
    let deadline = crate::arch::riscv64::csr::read_time().wrapping_add(cycles_per_tick);
    crate::arch::riscv64::sbi::set_timer(deadline).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{
        CYCLES_PER_TICK, TIMEBASE_HZ, quantum_elapsed, realtime_millis_at, sleep_deadline,
        ticks_to_millis,
    };

    // Catches the wall clock drifting from the timer or overflowing: the
    // RTC origin must advance by exactly the elapsed `time` CSR cycles
    // converted at the timebase, keep sub-millisecond origins, and survive
    // a counter that wrapped since boot.
    #[test]
    fn realtime_advances_the_rtc_origin_by_elapsed_cycles() {
        // 2001-02-03 04:05:06.789 UTC。
        let origin = 981_173_106_789_000_000;
        assert_eq!(
            realtime_millis_at(origin, 500, 500, TIMEBASE_HZ),
            981_173_106_789
        );
        // 10 MHzで1.5秒分のcycle。
        assert_eq!(
            realtime_millis_at(origin, 500, 500 + 15_000_000, TIMEBASE_HZ),
            981_173_108_289
        );
        // 端数のnanosecondは経過分と合わせてから切り捨てる。
        assert_eq!(realtime_millis_at(999_999, 0, 1, 1_000_000_000), 1);
        // 起動後に`time` CSRがwrapしても差は正しい。
        assert_eq!(realtime_millis_at(0, u64::MAX - 9_999, 0, TIMEBASE_HZ), 1);
        // 何年も動き続けたcycle数でも途中で溢れない。
        assert_eq!(
            realtime_millis_at(0, 0, 10_000_000 * 86_400 * 365 * 50, TIMEBASE_HZ),
            1_000 * 86_400 * 365 * 50
        );
    }

    // Catches a tick that was already pending at dispatch preempting the
    // newly dispatched process: only a full tick of run time ends a quantum.
    #[test]
    fn a_quantum_ends_only_after_a_full_tick() {
        assert!(!quantum_elapsed(1_000, 1_000, CYCLES_PER_TICK));
        assert!(!quantum_elapsed(
            1_000,
            1_000 + CYCLES_PER_TICK - 1,
            CYCLES_PER_TICK
        ));
        assert!(quantum_elapsed(
            1_000,
            1_000 + CYCLES_PER_TICK,
            CYCLES_PER_TICK
        ));
        assert!(quantum_elapsed(
            u64::MAX - 10,
            CYCLES_PER_TICK,
            CYCLES_PER_TICK
        ));
    }

    // Catches sleep waking early: the tick count must round up and add one
    // for the partially elapsed current tick.
    #[test]
    fn sleep_deadline_rounds_up_past_the_current_tick() {
        assert_eq!(sleep_deadline(7, 1), 9);
        assert_eq!(sleep_deadline(7, 10), 9);
        assert_eq!(sleep_deadline(7, 11), 10);
        assert_eq!(sleep_deadline(7, 50), 13);
        assert_eq!(sleep_deadline(u64::MAX, usize::MAX), u64::MAX);
    }

    #[test]
    fn converts_ticks_to_milliseconds() {
        assert_eq!(ticks_to_millis(0), 0);
        assert_eq!(ticks_to_millis(1), 10);
        assert_eq!(ticks_to_millis(250), 2_500);
    }
}
