//! virtio-mmio (modern interface v2) の共通部分。register file offset、
//! MMIOアクセス境界[`Mmio`]、virtqueueのdesc/avail/used ring、statusと
//! desc flagの定数をblock・netの両driverで共有する。
//!
//! queue/request領域は呼び出し側が所有する4 KiB整列領域とし、kernelの
//! 恒等mapにより領域アドレスがそのままdeviceへ渡す物理アドレスになる。

use core::sync::atomic::{Ordering, fence};

/// MMIO register fileのoffset (virtio-mmio v2)。
pub mod reg {
    pub const MAGIC: usize = 0x00;
    pub const VERSION: usize = 0x04;
    pub const DEVICE_ID: usize = 0x08;
    pub const DEVICE_FEATURES: usize = 0x10;
    pub const DEVICE_FEATURES_SEL: usize = 0x14;
    pub const DRIVER_FEATURES: usize = 0x20;
    pub const DRIVER_FEATURES_SEL: usize = 0x24;
    pub const QUEUE_SEL: usize = 0x30;
    pub const QUEUE_NUM_MAX: usize = 0x34;
    pub const QUEUE_NUM: usize = 0x38;
    pub const QUEUE_READY: usize = 0x44;
    pub const QUEUE_NOTIFY: usize = 0x50;
    pub const INTERRUPT_STATUS: usize = 0x60;
    pub const INTERRUPT_ACK: usize = 0x64;
    pub const STATUS: usize = 0x70;
    pub const QUEUE_DESC_LO: usize = 0x80;
    pub const QUEUE_DESC_HI: usize = 0x84;
    pub const QUEUE_DRIVER_LO: usize = 0x90;
    pub const QUEUE_DRIVER_HI: usize = 0x94;
    pub const QUEUE_DEVICE_LO: usize = 0xa0;
    pub const QUEUE_DEVICE_HI: usize = 0xa4;
    /// device config領域の起点。以降のlayoutはdevice種別ごとに異なる。
    pub const CONFIG: usize = 0x100;
}

pub const MMIO_MAGIC: u32 = 0x7472_6976;
pub const MMIO_VERSION: u32 = 2;

pub const STATUS_ACK: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FEATURES_OK: u32 = 8;

/// VIRTIO_F_VERSION_1。features high wordのbit 0 (=bit 32)。
pub const FEATURE_VERSION_1_HI: u32 = 1;

/// virtqueueの長さ。desc(16B)×8で128 byteと小さく、1 page内にqueueの
/// 3領域とrequest/data bufferを収められる上限である。
pub const QUEUE_LEN: usize = 8;
pub const DESC_NEXT: u16 = 1;
pub const DESC_WRITE: u16 = 2;

/// device応答を待つpollの上限。QEMU上では即応答するので、これは
/// device故障時の暴走防止である。
pub const POLL_LIMIT: u32 = 5_000_000;

/// device MMIO register fileへのアクセスを抽象化する。実機側はvolatile
/// 32-bit load/store、host testはregister配列+queue消化を真似る。
pub trait Mmio {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&mut self, offset: usize, value: u32);

    /// config spaceのbyte読み取り。既定は整列した32-bit loadから該当
    /// byteを取り出す。MAC address等の8-bit fieldを読むために使う。
    fn read8(&self, offset: usize) -> u8 {
        (self.read32(offset & !3) >> ((offset & 3) * 8)) as u8
    }

    /// deviceが立てたused-buffer割り込みをackする。割り込みを使う経路
    /// (PLIC claim) だけが呼び、polling経路では不要である。
    fn interrupt_ack(&mut self) {
        let status = self.read32(reg::INTERRUPT_STATUS);
        self.write32(reg::INTERRUPT_ACK, status);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Desc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C)]
pub struct AvailRing {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; QUEUE_LEN],
    pub used_event: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C)]
pub struct UsedRing {
    pub flags: u16,
    pub idx: u16,
    pub elems: [UsedElem; QUEUE_LEN],
    pub avail_event: u16,
}

/// 1 queueのdesc/avail/usedの3領域。各deviceのregion構造がフィールド
/// として持ち、`setup_queue`がこの構造の物理アドレスを登録する。
#[repr(C)]
pub struct QueueRings {
    pub desc: [Desc; QUEUE_LEN],
    pub avail: AvailRing,
    pub used: UsedRing,
}

/// queue `index`へ`desc`/`avail`/`used`の3領域を登録しreadyにする。
/// 各領域はdevice固有region内の配置を指す物理アドレスで、連続している
/// 必要はない。device最大値が`min_len`未満なら`None`を返し、呼び出し
/// 側がdevice固有のerrorへ写像する。戻り値は実際に登録したqueue長
/// （`QUEUE_LEN`とdevice最大の小さい方）。
pub fn setup_queue<M: Mmio>(
    mmio: &mut M,
    index: u32,
    desc: *const Desc,
    avail: *const AvailRing,
    used: *const UsedRing,
    min_len: usize,
) -> Option<u16> {
    mmio.write32(reg::QUEUE_SEL, index);
    let max = mmio.read32(reg::QUEUE_NUM_MAX) as usize;
    if max < min_len {
        return None;
    }
    let queue_len = max.min(QUEUE_LEN);
    mmio.write32(reg::QUEUE_NUM, queue_len as u32);
    let mut write_addr = |lo: usize, hi: usize, addr: u64| {
        mmio.write32(lo, addr as u32);
        mmio.write32(hi, (addr >> 32) as u32);
    };
    write_addr(reg::QUEUE_DESC_LO, reg::QUEUE_DESC_HI, desc as u64);
    write_addr(reg::QUEUE_DRIVER_LO, reg::QUEUE_DRIVER_HI, avail as u64);
    write_addr(reg::QUEUE_DEVICE_LO, reg::QUEUE_DEVICE_HI, used as u64);
    // 領域の初期化内容をdeviceのqueue読み取りより先に可視化する。
    fence(Ordering::SeqCst);
    mmio.write32(reg::QUEUE_READY, 1);
    Some(queue_len as u16)
}

/// `begin_init`の失敗理由。`WrongDeviceId`は期待と違うdevice idを
/// 値として持ち、呼び出し側がdevice固有のerror variantへ写像する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    BadMagic,
    UnsupportedVersion(u32),
    WrongDeviceId(u32),
    Timeout,
}

/// magic値・version・device idの検証とreset→ACK→DRIVERの遷移。
/// feature negotiationの直前まで共通手順を進める。
pub fn begin_init<M: Mmio>(mmio: &mut M, expected_device_id: u32) -> Result<(), InitError> {
    if mmio.read32(reg::MAGIC) != MMIO_MAGIC {
        return Err(InitError::BadMagic);
    }
    let version = mmio.read32(reg::VERSION);
    if version != MMIO_VERSION {
        return Err(InitError::UnsupportedVersion(version));
    }
    let device_id = mmio.read32(reg::DEVICE_ID);
    if device_id != expected_device_id {
        return Err(InitError::WrongDeviceId(device_id));
    }

    // reset → ACK → DRIVER
    mmio.write32(reg::STATUS, 0);
    let mut spins = 0u32;
    while mmio.read32(reg::STATUS) != 0 {
        spins += 1;
        if spins > POLL_LIMIT {
            return Err(InitError::Timeout);
        }
    }
    mmio.write32(reg::STATUS, STATUS_ACK);
    mmio.write32(reg::STATUS, STATUS_ACK | STATUS_DRIVER);
    Ok(())
}
