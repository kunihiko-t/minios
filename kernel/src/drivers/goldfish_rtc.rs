//! QEMU `virt`のGoldfish RTC（`google,goldfish-rtc`）。
//!
//! RTCは64 bitのUnix時刻をnanosecondで持つ。`TIME_LOW`を読むと値が確定し、
//! 上位32 bitが`TIME_HIGH`へlatchされるため、必ず`TIME_LOW`から読む。
//! kernelは起動時のbare mode（VA==PA）で1回だけ読み、以後は`time` CSRで
//! 時刻を進めるので、このregisterをpage tableへmapしない。

/// 下位32 bit。読むと上位32 bitが`TIME_HIGH`へlatchされる。
const TIME_LOW: usize = 0x00;
/// `TIME_LOW`を読んだ時点の上位32 bit。
const TIME_HIGH: usize = 0x04;

/// RTCが示すUnix時刻をnanosecondで返す。
///
/// # Safety
///
/// `base`はFDTが報告したGoldfish RTCのMMIO baseで、呼び出し時点で
/// そのaddressへアクセスできなければならない（paging有効化前のbare mode）。
pub unsafe fn read_unix_nanos(base: usize) -> u64 {
    // Safety: 呼び出し側が`base`のMMIO registerへのアクセスを保証する。
    // `TIME_LOW`を先に読むことで`TIME_HIGH`が同じ瞬間の上位bitになる。
    let low = unsafe { core::ptr::read_volatile((base + TIME_LOW) as *const u32) };
    // Safety: 同上。
    let high = unsafe { core::ptr::read_volatile((base + TIME_HIGH) as *const u32) };
    (u64::from(high) << 32) | u64::from(low)
}
