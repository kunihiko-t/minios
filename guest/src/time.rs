//! 実時刻（Unix時刻）と、そのUTCの暦への変換。
//!
//! 起動からの経過時間は`process::clock_ms`、実時刻はここの`realtime_ms`で
//! 読む。kernelは起動時にRTCを1回だけ読み、以後は`time` CSRで時刻を進める。
//! 暦への変換はkernelのFAT32 timestampと共有する`minios_abi::time`にある。

pub use minios_abi::time::DateTime;

use crate::{Result, check, sys};

/// Unix epoch（1970-01-01 00:00:00 UTC）からの経過millisecond。RTCを持たない
/// machineでは`ENODEV`。
pub fn realtime_ms() -> Result<u64> {
    check(sys::sys_realtime()).map(|millis| millis as u64)
}
