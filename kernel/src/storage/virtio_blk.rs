//! virtio-mmio block device (modern interface v2)。read-only・単一queue・
//! 完了はused ringのpollで待つ。PLICを必要としない。
//!
//! register fileは[`Mmio`] traitで抽象化し、queue/request領域は呼び出し側が
//! 所有する4 KiB整列の[`VirtioRegion`]とする。kernelはmanaged RAMを恒等map
//! するため、領域のアドレスがそのままdeviceへ渡す物理アドレスになる。

use core::mem::offset_of;
use core::ops::DerefMut;
use core::sync::atomic::{Ordering, fence};

use crate::storage::SectorReader;

const MMIO_MAGIC: u32 = 0x7472_6976;
const MMIO_VERSION: u32 = 2;
const DEVICE_ID_BLOCK: u32 = 2;

mod reg {
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
    pub const CONFIG: usize = 0x100;
}

const STATUS_ACK: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;

/// VIRTIO_F_VERSION_1。features high wordのbit 0 (=bit 32)。
const FEATURE_VERSION_1_HI: u32 = 1;

const QUEUE_LEN: usize = 8;
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;

const BLK_REQUEST_IN: u32 = 0;
const BLK_STATUS_OK: u8 = 0;

/// device応答を待つpollの上限。QEMU上では即応答するので、これは
/// device故障時の暴走防止である。
const POLL_LIMIT: u32 = 5_000_000;

/// device MMIO register fileへのアクセスを抽象化する。実機側はvolatile
/// 32-bit load/store、host testはregister配列+queue消化を真似る。
pub trait Mmio {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&mut self, offset: usize, value: u32);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
struct AvailRing {
    flags: u16,
    idx: u16,
    ring: [u16; QUEUE_LEN],
    used_event: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UsedElem {
    id: u32,
    len: u32,
}

#[repr(C)]
struct UsedRing {
    flags: u16,
    idx: u16,
    elems: [UsedElem; QUEUE_LEN],
    avail_event: u16,
}

#[repr(C)]
struct RequestHeader {
    request_type: u32,
    reserved: u32,
    sector: u64,
}

/// queue (desc/avail/used) とrequest header/data bufferを1 pageに収める
/// DMA領域。`align(4096)`により全体が1 page内に納まり、desc(16B)・
/// avail(2B)・used(4B)の各整列要求を満たす。
#[repr(C, align(4096))]
pub struct VirtioRegion {
    desc: [Desc; QUEUE_LEN],
    avail: AvailRing,
    used: UsedRing,
    request: RequestHeader,
    status: u8,
    data: [u8; 512],
}

impl VirtioRegion {
    /// 領域のベースアドレス。恒等mapの下で物理アドレスとして使う。
    fn base(&self) -> usize {
        self as *const Self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    BadMagic,
    UnsupportedVersion(u32),
    NotBlockDevice(u32),
    FeatureRejected,
    QueueTooSmall,
    /// 渡された領域が4 KiB整列していない。
    MisalignedRegion,
    /// requestしたsectorがdeviceの容量を超えた。
    BadLba,
    /// used ringが`POLL_LIMIT`回のpoll内に進まなかった。
    Timeout,
    /// deviceがqueueへ期待と違うdescriptor idを返した。
    BadUsedId(u32),
    /// request status byteが0以外 (1=IOERR, 2=UNSUPPORTED)。
    DeviceStatus(u8),
}

pub struct VirtioBlk<M: Mmio, R: DerefMut<Target = VirtioRegion>> {
    mmio: M,
    region: R,
    queue_len: u16,
    capacity: u64,
    next_avail: u16,
    last_used: u16,
}

impl<M: Mmio, R: DerefMut<Target = VirtioRegion>> VirtioBlk<M, R> {
    /// virtio v2の初期化手順を実行する。失敗時は`region`をdropし、
    /// 呼び出し側の所有権型 (frame wrapper等) が供給元へ返す。
    pub fn init(mut mmio: M, region: R) -> Result<Self, VirtioError> {
        if !region.base().is_multiple_of(4096) {
            return Err(VirtioError::MisalignedRegion);
        }
        if mmio.read32(reg::MAGIC) != MMIO_MAGIC {
            return Err(VirtioError::BadMagic);
        }
        let version = mmio.read32(reg::VERSION);
        if version != MMIO_VERSION {
            return Err(VirtioError::UnsupportedVersion(version));
        }
        let device_id = mmio.read32(reg::DEVICE_ID);
        if device_id != DEVICE_ID_BLOCK {
            return Err(VirtioError::NotBlockDevice(device_id));
        }

        // reset → ACK → DRIVER
        mmio.write32(reg::STATUS, 0);
        let mut spins = 0u32;
        while mmio.read32(reg::STATUS) != 0 {
            spins += 1;
            if spins > POLL_LIMIT {
                return Err(VirtioError::Timeout);
            }
        }
        mmio.write32(reg::STATUS, STATUS_ACK);
        mmio.write32(reg::STATUS, STATUS_ACK | STATUS_DRIVER);

        // feature negotiation: VERSION_1だけを要求する。
        mmio.write32(reg::DEVICE_FEATURES_SEL, 0);
        let _features_lo = mmio.read32(reg::DEVICE_FEATURES);
        mmio.write32(reg::DEVICE_FEATURES_SEL, 1);
        let features_hi = mmio.read32(reg::DEVICE_FEATURES);
        if features_hi & FEATURE_VERSION_1_HI == 0 {
            return Err(VirtioError::FeatureRejected);
        }
        mmio.write32(reg::DRIVER_FEATURES_SEL, 0);
        mmio.write32(reg::DRIVER_FEATURES, 0);
        mmio.write32(reg::DRIVER_FEATURES_SEL, 1);
        mmio.write32(reg::DRIVER_FEATURES, FEATURE_VERSION_1_HI);
        mmio.write32(reg::STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
        if mmio.read32(reg::STATUS) & STATUS_FEATURES_OK == 0 {
            return Err(VirtioError::FeatureRejected);
        }

        // queue 0を登録する。requestには3 descあれば足りる。
        mmio.write32(reg::QUEUE_SEL, 0);
        let max = mmio.read32(reg::QUEUE_NUM_MAX) as usize;
        if max < 3 {
            return Err(VirtioError::QueueTooSmall);
        }
        let queue_len = max.min(QUEUE_LEN);
        mmio.write32(reg::QUEUE_NUM, queue_len as u32);
        let base = region.base() as u64;
        let write_addr = |mmio: &mut M, lo: usize, hi: usize, addr: u64| {
            mmio.write32(lo, addr as u32);
            mmio.write32(hi, (addr >> 32) as u32);
        };
        write_addr(
            &mut mmio,
            reg::QUEUE_DESC_LO,
            reg::QUEUE_DESC_HI,
            base + offset_of!(VirtioRegion, desc) as u64,
        );
        write_addr(
            &mut mmio,
            reg::QUEUE_DRIVER_LO,
            reg::QUEUE_DRIVER_HI,
            base + offset_of!(VirtioRegion, avail) as u64,
        );
        write_addr(
            &mut mmio,
            reg::QUEUE_DEVICE_LO,
            reg::QUEUE_DEVICE_HI,
            base + offset_of!(VirtioRegion, used) as u64,
        );
        // 領域の初期化内容をdeviceのqueue読み取りより先に可視化する。
        fence(Ordering::SeqCst);
        mmio.write32(reg::QUEUE_READY, 1);

        // block config: capacity (sectors) が先頭8 byte。
        let capacity =
            mmio.read32(reg::CONFIG) as u64 | (mmio.read32(reg::CONFIG + 4) as u64) << 32;

        mmio.write32(
            reg::STATUS,
            STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );

        Ok(Self {
            mmio,
            region,
            queue_len: queue_len as u16,
            capacity,
            next_avail: 0,
            last_used: 0,
        })
    }

    pub fn capacity_sectors(&self) -> u64 {
        self.capacity
    }

    fn read_sector_into(&mut self, lba: u32) -> Result<(), VirtioError> {
        let base = self.region.base();
        unsafe {
            core::ptr::write_volatile(
                &raw mut self.region.request,
                RequestHeader {
                    request_type: BLK_REQUEST_IN,
                    reserved: 0,
                    sector: u64::from(lba),
                },
            );
            core::ptr::write_volatile(
                &raw mut self.region.desc[0],
                Desc {
                    addr: (base + offset_of!(VirtioRegion, request)) as u64,
                    len: 16,
                    flags: DESC_NEXT,
                    next: 1,
                },
            );
            core::ptr::write_volatile(
                &raw mut self.region.desc[1],
                Desc {
                    addr: (base + offset_of!(VirtioRegion, data)) as u64,
                    len: 512,
                    flags: DESC_NEXT | DESC_WRITE,
                    next: 2,
                },
            );
            core::ptr::write_volatile(
                &raw mut self.region.desc[2],
                Desc {
                    addr: (base + offset_of!(VirtioRegion, status)) as u64,
                    len: 1,
                    flags: DESC_WRITE,
                    next: 0,
                },
            );
            core::ptr::write_volatile(&raw mut self.region.status, 0xff);
            core::ptr::write_volatile(
                &raw mut self.region.avail.ring[(self.next_avail % self.queue_len) as usize],
                0,
            );
        }
        // desc/buffer更新をavail idx公開の前にdeviceへ見せる。
        fence(Ordering::Release);
        unsafe {
            core::ptr::write_volatile(
                &raw mut self.region.avail.idx,
                self.next_avail.wrapping_add(1),
            );
        }
        self.next_avail = self.next_avail.wrapping_add(1);
        // MMIO notifyがavail更新より先にdeviceへ届かないようにする。
        fence(Ordering::SeqCst);
        self.mmio.write32(reg::QUEUE_NOTIFY, 0);

        let mut spins = 0u32;
        loop {
            let used_idx = unsafe { core::ptr::read_volatile(&raw const self.region.used.idx) };
            if used_idx != self.last_used {
                break;
            }
            spins += 1;
            if spins > POLL_LIMIT {
                return Err(VirtioError::Timeout);
            }
        }
        // deviceのdata/status書き込みを読む前に完了を確定する。
        fence(Ordering::Acquire);
        let elem = unsafe {
            core::ptr::read_volatile(
                &raw const self.region.used.elems[(self.last_used % self.queue_len) as usize],
            )
        };
        self.last_used = self.last_used.wrapping_add(1);

        // 使ったbuffered interruptをackする (v2はused buffer notifyが既定)。
        let pending = self.mmio.read32(reg::INTERRUPT_STATUS);
        self.mmio.write32(reg::INTERRUPT_ACK, pending);

        if elem.id != 0 {
            return Err(VirtioError::BadUsedId(elem.id));
        }
        let status = unsafe { core::ptr::read_volatile(&raw const self.region.status) };
        if status != BLK_STATUS_OK {
            return Err(VirtioError::DeviceStatus(status));
        }
        Ok(())
    }
}

impl<M: Mmio, R: DerefMut<Target = VirtioRegion>> SectorReader for VirtioBlk<M, R> {
    type Error = VirtioError;

    fn read_sector(&mut self, lba: u32, destination: &mut [u8; 512]) -> Result<(), VirtioError> {
        if u64::from(lba) >= self.capacity {
            return Err(VirtioError::BadLba);
        }
        self.read_sector_into(lba)?;
        destination.copy_from_slice(&self.region.data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::alloc::{Layout, alloc_zeroed, dealloc};
    use std::vec::Vec;

    use super::{Mmio, VirtioBlk, VirtioError, VirtioRegion, reg};
    use crate::storage::SectorReader;

    /// host test用の4 KiB整列領域。deviceがDMA先として解釈するため、
    /// `VirtioRegion`の型整列 (4 KiB) をLayoutでも保証する。
    struct TestRegion {
        ptr: *mut VirtioRegion,
    }

    impl TestRegion {
        fn new() -> Self {
            let layout =
                Layout::from_size_align(core::mem::size_of::<VirtioRegion>(), 4096).unwrap();
            let ptr = unsafe { alloc_zeroed(layout) } as *mut VirtioRegion;
            assert!(!ptr.is_null());
            Self { ptr }
        }
    }

    impl core::ops::Deref for TestRegion {
        type Target = VirtioRegion;
        fn deref(&self) -> &VirtioRegion {
            unsafe { &*self.ptr }
        }
    }

    impl core::ops::DerefMut for TestRegion {
        fn deref_mut(&mut self) -> &mut VirtioRegion {
            unsafe { &mut *self.ptr }
        }
    }

    impl Drop for TestRegion {
        fn drop(&mut self) {
            let layout =
                Layout::from_size_align(core::mem::size_of::<VirtioRegion>(), 4096).unwrap();
            unsafe { dealloc(self.ptr as *mut u8, layout) };
        }
    }

    /// register fileとqueue消化を真似るfake device。QUEUE_NOTIFYへのwriteで
    /// guestのdesc chainを解釈し、data/statusを書き戻してused ringを進める。
    struct FakeDevice {
        regs: [u32; 64],
        config: [u32; 64],
        magic: u32,
        version: u32,
        device_id: u32,
        features_hi: u32,
        queue_num_max: u32,
        accept_features_ok: bool,
        driver_features_hi: u32,
        device_features_sel: u32,
        desc_addr: usize,
        driver_addr: usize,
        device_addr: usize,
        sector_data: Vec<u8>,
        result_status: u8,
        used_id: u32,
        never_complete: bool,
    }

    impl FakeDevice {
        fn new(capacity: u64) -> Self {
            let mut device = Self {
                regs: [0; 64],
                config: [0; 64],
                magic: super::MMIO_MAGIC,
                version: super::MMIO_VERSION,
                device_id: super::DEVICE_ID_BLOCK,
                features_hi: super::FEATURE_VERSION_1_HI,
                queue_num_max: super::QUEUE_LEN as u32,
                accept_features_ok: true,
                driver_features_hi: 0,
                device_features_sel: 0,
                desc_addr: 0,
                driver_addr: 0,
                device_addr: 0,
                sector_data: std::vec![0u8; 512],
                result_status: 0,
                used_id: 0,
                never_complete: false,
            };
            device.config[0] = capacity as u32;
            device.config[1] = (capacity >> 32) as u32;
            device
        }

        fn complete_request(&mut self) {
            if self.never_complete {
                return;
            }
            unsafe {
                let used = &mut *(self.device_addr as *mut super::UsedRing);
                let descs = &*(self.desc_addr as *const [super::Desc; super::QUEUE_LEN]);
                let header = &*(descs[0].addr as *const super::RequestHeader);

                assert_eq!(header.request_type, super::BLK_REQUEST_IN);
                assert_eq!(descs[0].flags, super::DESC_NEXT);
                assert_eq!(descs[1].flags, super::DESC_NEXT | super::DESC_WRITE);
                assert_eq!(descs[1].len, 512);
                assert_eq!(descs[2].flags, super::DESC_WRITE);
                assert_eq!(descs[2].len, 1);

                core::ptr::copy_nonoverlapping(
                    self.sector_data.as_ptr(),
                    descs[1].addr as *mut u8,
                    512,
                );
                *(descs[2].addr as *mut u8) = self.result_status;

                let slot = (used.idx % super::QUEUE_LEN as u16) as usize;
                used.elems[slot] = super::UsedElem {
                    id: self.used_id,
                    len: 512 + 17,
                };
                used.idx = used.idx.wrapping_add(1);
                self.regs[reg::INTERRUPT_STATUS / 4] = 1;
            }
        }
    }

    impl Mmio for FakeDevice {
        fn read32(&self, offset: usize) -> u32 {
            match offset {
                reg::MAGIC => self.magic,
                reg::VERSION => self.version,
                reg::DEVICE_ID => self.device_id,
                reg::DEVICE_FEATURES => {
                    if self.device_features_sel == 0 {
                        0
                    } else {
                        self.features_hi
                    }
                }
                reg::QUEUE_NUM_MAX => self.queue_num_max,
                reg::STATUS => self.regs[reg::STATUS / 4],
                reg::INTERRUPT_STATUS => self.regs[reg::INTERRUPT_STATUS / 4],
                o if o >= reg::CONFIG => self.config[(o - reg::CONFIG) / 4],
                _ => 0,
            }
        }

        fn write32(&mut self, offset: usize, value: u32) {
            match offset {
                reg::STATUS => {
                    // FEATURES_OKを立てようとして受理不能なfeatureを要求した
                    // 場合はdeviceがFEATURES_OKを落とす。
                    let mut stored = value;
                    if value & super::STATUS_FEATURES_OK != 0 && !self.accept_features_ok {
                        stored &= !super::STATUS_FEATURES_OK;
                    }
                    self.regs[reg::STATUS / 4] = stored;
                }
                reg::DEVICE_FEATURES_SEL => self.device_features_sel = value,
                reg::DRIVER_FEATURES_SEL => {}
                reg::DRIVER_FEATURES => self.driver_features_hi = value,
                reg::QUEUE_DESC_LO => {
                    self.desc_addr = (self.desc_addr & !0xffff_ffff) | value as usize;
                }
                reg::QUEUE_DRIVER_LO => {
                    self.driver_addr = (self.driver_addr & !0xffff_ffff) | value as usize;
                }
                reg::QUEUE_DEVICE_LO => {
                    self.device_addr = (self.device_addr & !0xffff_ffff) | value as usize;
                }
                reg::QUEUE_DESC_HI => {
                    self.desc_addr = (self.desc_addr & 0xffff_ffff) | (value as usize) << 32;
                }
                reg::QUEUE_DRIVER_HI => {
                    self.driver_addr = (self.driver_addr & 0xffff_ffff) | (value as usize) << 32;
                }
                reg::QUEUE_DEVICE_HI => {
                    self.device_addr = (self.device_addr & 0xffff_ffff) | (value as usize) << 32;
                }
                reg::QUEUE_NUM => {}
                reg::QUEUE_READY => {}
                reg::QUEUE_SEL => {}
                reg::INTERRUPT_ACK => self.regs[reg::INTERRUPT_STATUS / 4] &= !value,
                reg::QUEUE_NOTIFY => self.complete_request(),
                _ => {}
            }
        }
    }

    fn device_and_region() -> (FakeDevice, TestRegion) {
        (FakeDevice::new(4096), TestRegion::new())
    }

    // Catches skipping a handshake step, leaving FEATURES_OK unset, or never
    // polling the used ring to completion.
    #[test]
    fn init_negotiates_and_read_sector_returns_device_data() {
        let (mut device, region) = device_and_region();
        for (i, byte) in device.sector_data.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        let mut blk = VirtioBlk::init(device, region).unwrap();
        assert_eq!(blk.capacity_sectors(), 4096);

        let mut buf = [0u8; 512];
        blk.read_sector(0, &mut buf).unwrap();
        assert_eq!(buf[0], 0);
        assert_eq!(buf[250], 250);
        assert_eq!(buf[511], (511 % 251) as u8);
    }

    // Catches wrapping logic that forgets ring slots advance independently of
    // the descriptor head used per request.
    #[test]
    fn consecutive_reads_advance_avail_and_used_indices() {
        let (device, region) = device_and_region();
        let mut blk = VirtioBlk::init(device, region).unwrap();

        for _ in 0..3 {
            let mut buf = [0u8; 512];
            blk.read_sector(7, &mut buf).unwrap();
        }
        let avail_idx = unsafe { core::ptr::read_volatile(&raw const blk.region.avail.idx) };
        let used_idx = unsafe { core::ptr::read_volatile(&raw const blk.region.used.idx) };
        assert_eq!(avail_idx, 3);
        assert_eq!(used_idx, 3);
    }

    // Catches accepting a device with a corrupt register file.
    #[test]
    fn init_rejects_bad_magic_version_and_non_block_devices() {
        let (mut device, region) = device_and_region();
        device.magic = 0;
        assert_eq!(
            VirtioBlk::init(device, region).err().unwrap(),
            VirtioError::BadMagic
        );

        let (mut device, region) = device_and_region();
        device.version = 1;
        assert_eq!(
            VirtioBlk::init(device, region).err().unwrap(),
            VirtioError::UnsupportedVersion(1)
        );

        let (mut device, region) = device_and_region();
        device.device_id = 4; // RNG
        assert_eq!(
            VirtioBlk::init(device, region).err().unwrap(),
            VirtioError::NotBlockDevice(4)
        );
    }

    // Catches negotiating legacy-only devices or ignoring a FEATURES_OK
    // rejection.
    #[test]
    fn init_rejects_missing_version_1_and_failed_features_ok() {
        let (mut device, region) = device_and_region();
        device.features_hi = 0;
        assert_eq!(
            VirtioBlk::init(device, region).err().unwrap(),
            VirtioError::FeatureRejected
        );

        let (mut device, region) = device_and_region();
        device.accept_features_ok = false;
        assert_eq!(
            VirtioBlk::init(device, region).err().unwrap(),
            VirtioError::FeatureRejected
        );
    }

    // Catches requesting a descriptor chain the queue cannot hold.
    #[test]
    fn init_rejects_a_queue_too_small_for_a_request() {
        let (mut device, region) = device_and_region();
        device.queue_num_max = 2;
        assert_eq!(
            VirtioBlk::init(device, region).err().unwrap(),
            VirtioError::QueueTooSmall
        );
    }

    // Catches reading past the reported capacity or skipping the status byte.
    #[test]
    fn read_sector_bounds_lba_and_propagates_device_status() {
        let (device, region) = device_and_region();
        let mut blk = VirtioBlk::init(device, region).unwrap();
        let mut buf = [0u8; 512];
        assert_eq!(blk.read_sector(4096, &mut buf), Err(VirtioError::BadLba));

        let (mut device, region) = device_and_region();
        device.result_status = 1; // VIRTIO_BLK_S_IOERR
        let mut blk = VirtioBlk::init(device, region).unwrap();
        assert_eq!(
            blk.read_sector(0, &mut buf),
            Err(VirtioError::DeviceStatus(1))
        );
    }

    // Catches a missing poll bound or a missing used-id validation.
    #[test]
    fn read_sector_times_out_and_validates_used_id() {
        let (mut device, region) = device_and_region();
        device.never_complete = true;
        let mut blk = VirtioBlk::init(device, region).unwrap();
        let mut buf = [0u8; 512];
        assert_eq!(blk.read_sector(0, &mut buf), Err(VirtioError::Timeout));

        let (mut device, region) = device_and_region();
        device.used_id = 5;
        let mut blk = VirtioBlk::init(device, region).unwrap();
        assert_eq!(blk.read_sector(0, &mut buf), Err(VirtioError::BadUsedId(5)));
    }
}
