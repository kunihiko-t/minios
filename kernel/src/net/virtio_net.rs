//! virtio-mmio network device (modern interface v2)。rx(queue 0)と
//! tx(queue 1)の2 queueでethernet frameを送受する。rx bufferは初期化
//! 時にすべてdeviceへpostし、完了(used)を`drain_rx`が回収して再post
//! する。txは1件ずつboundedに完了を待つ。PLICを必要としない
//! （used-buffer割り込みはkernel側がPLICでenableするだけである）。
//!
//! register fileは[`crate::virtio::Mmio`] traitで抽象化し、queue領域は
//! 呼び出し側が所有する4 KiB整列の[`NetQueueRegion`]と、rx bufferの
//! 4枚の[`NetRxBufPage`]を使う。kernelはmanaged RAMを恒等mapするため、
//! 領域のアドレスがそのままdeviceへ渡す物理アドレスになる。

use core::ops::DerefMut;
use core::sync::atomic::{Ordering, fence};

use crate::virtio::{
    self, DESC_WRITE, Desc, FEATURE_VERSION_1_HI, Mmio, POLL_LIMIT, QueueRings, STATUS_ACK,
    STATUS_DRIVER, STATUS_DRIVER_OK, STATUS_FEATURES_OK, reg,
};

const DEVICE_ID_NET: u32 = 1;
const RX_QUEUE: u32 = 0;
const TX_QUEUE: u32 = 1;

/// VIRTIO_NET_F_MAC。features low wordのbit 5。config領域のMAC addressを
/// 読むためにnegotiationで必須とする。
const FEATURE_MAC_LO: u32 = 1 << 5;

/// VIRTIO_NET_F_MRG_RXBUF。features low wordのbit 15。rx bufferが常に
/// MTUを収める大きさなのでmergeableでも1 packet=1 descのまま処理でき、
/// virtio_net_hdrの長さがdeviceとdriverで12 byte (num_buffers付き) に
/// 一致するようnegotiateする。
const FEATURE_MRG_RXBUF_LO: u32 = 1 << 15;

/// virtio_net_hdrの長さ。MRG_RXBUFをnegotiateするためnum_buffers付きの
/// 12 byte固定。
pub const NET_HDR_LEN: usize = 12;
/// ethernet MTU 1500 + header 14。FCS/VLAN tagはdriverが触れない。
pub const ETH_FRAME_MAX: usize = 1514;
/// rx desc 1本へdeviceが書き込む領域の長さ (virtio_net_hdr + frame)。
pub const RX_BUF_LEN: usize = NET_HDR_LEN + ETH_FRAME_MAX;
/// 1 pageに収めるrx buffer数。
pub const RX_BUFS_PER_PAGE: usize = 2;
/// rx bufferを構成するpage数。4 pageで`QUEUE_LEN`と同じ8 bufferを持つ。
pub const RX_BUF_PAGES: usize = 4;
/// 常時deviceへpostするrx buffer数。
pub const RX_BUFS: usize = RX_BUFS_PER_PAGE * RX_BUF_PAGES;

/// rx/tx両queueのringとtxの送信bufferを1 pageに収めるDMA領域。
/// `align(4096)`により全体が1 page内に納まり、desc(16B)・avail(2B)・
/// used(4B)の各整列要求を満たす。
#[repr(C, align(4096))]
pub struct NetQueueRegion {
    pub rx: QueueRings,
    pub tx: QueueRings,
    tx_hdr: [u8; NET_HDR_LEN],
    tx_frame: [u8; ETH_FRAME_MAX],
}

impl NetQueueRegion {
    /// 領域のベースアドレス。恒等mapの下で物理アドレスとして使う。
    fn base(&self) -> usize {
        self as *const Self as usize
    }
}

/// rx buffer 2個分の1 page。descがbufferの物理アドレスを個別に指す
/// ため、page同士が連続している必要はない。
#[repr(C, align(4096))]
pub struct NetRxBufPage {
    bufs: [[u8; RX_BUF_LEN]; RX_BUFS_PER_PAGE],
    _reserved: [u8; 4096 - RX_BUFS_PER_PAGE * RX_BUF_LEN],
}

impl NetRxBufPage {
    /// 領域のベースアドレス。恒等mapの下で物理アドレスとして使う。
    fn base(&self) -> usize {
        self as *const Self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    BadMagic,
    UnsupportedVersion(u32),
    NotNetDevice(u32),
    FeatureRejected,
    /// deviceがVIRTIO_NET_F_MACを提供しない。
    NoMac,
    QueueTooSmall,
    /// 渡された領域が4 KiB整列していない。
    MisalignedRegion,
    /// used ringが`POLL_LIMIT`回のpoll内に進まなかった。
    Timeout,
    /// deviceがqueueへ期待と違うdescriptor idを返した。
    BadUsedId(u32),
    /// 送信frameが`ETH_FRAME_MAX`を超えた。
    FrameTooLarge,
}

impl From<virtio::InitError> for NetError {
    fn from(error: virtio::InitError) -> Self {
        match error {
            virtio::InitError::BadMagic => Self::BadMagic,
            virtio::InitError::UnsupportedVersion(version) => Self::UnsupportedVersion(version),
            virtio::InitError::WrongDeviceId(id) => Self::NotNetDevice(id),
            virtio::InitError::Timeout => Self::Timeout,
        }
    }
}

pub struct VirtioNet<
    M: Mmio,
    R: DerefMut<Target = NetQueueRegion>,
    B: DerefMut<Target = NetRxBufPage>,
> {
    mmio: M,
    region: R,
    rx_pages: [B; RX_BUF_PAGES],
    mac: [u8; 6],
    queue_len: u16,
    rx_avail_idx: u16,
    rx_last_used: u16,
    tx_avail_idx: u16,
    tx_last_used: u16,
    tx_in_flight: bool,
}

impl<M: Mmio, R: DerefMut<Target = NetQueueRegion>, B: DerefMut<Target = NetRxBufPage>>
    VirtioNet<M, R, B>
{
    /// virtio v2の初期化手順を実行し、rx queueへ全bufferをpostして
    /// DRIVER_OKでdeviceを起こす。失敗時は`region`と`rx_pages`をdropし、
    /// 呼び出し側の所有権型 (frame wrapper等) が供給元へ返す。
    pub fn init(mut mmio: M, region: R, rx_pages: [B; RX_BUF_PAGES]) -> Result<Self, NetError> {
        if !region.base().is_multiple_of(4096)
            || rx_pages
                .iter()
                .any(|page| !page.base().is_multiple_of(4096))
        {
            return Err(NetError::MisalignedRegion);
        }
        virtio::begin_init(&mut mmio, DEVICE_ID_NET)?;

        // feature negotiation: F_MAC + F_MRG_RXBUF + VERSION_1を要求する。
        mmio.write32(reg::DEVICE_FEATURES_SEL, 0);
        let features_lo = mmio.read32(reg::DEVICE_FEATURES);
        mmio.write32(reg::DEVICE_FEATURES_SEL, 1);
        let features_hi = mmio.read32(reg::DEVICE_FEATURES);
        if features_hi & FEATURE_VERSION_1_HI == 0 {
            return Err(NetError::FeatureRejected);
        }
        if features_lo & (FEATURE_MAC_LO | FEATURE_MRG_RXBUF_LO)
            != FEATURE_MAC_LO | FEATURE_MRG_RXBUF_LO
        {
            return Err(NetError::NoMac);
        }
        mmio.write32(reg::DRIVER_FEATURES_SEL, 0);
        mmio.write32(reg::DRIVER_FEATURES, FEATURE_MAC_LO | FEATURE_MRG_RXBUF_LO);
        mmio.write32(reg::DRIVER_FEATURES_SEL, 1);
        mmio.write32(reg::DRIVER_FEATURES, FEATURE_VERSION_1_HI);
        mmio.write32(reg::STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
        if mmio.read32(reg::STATUS) & STATUS_FEATURES_OK == 0 {
            return Err(NetError::FeatureRejected);
        }

        // device config先頭のMAC address (6 byte)。
        let mut mac = [0u8; 6];
        for (index, byte) in mac.iter_mut().enumerate() {
            *byte = mmio.read8(reg::CONFIG + index);
        }

        let mut device = Self {
            mmio,
            region,
            rx_pages,
            mac,
            queue_len: 0,
            rx_avail_idx: 0,
            rx_last_used: 0,
            tx_avail_idx: 0,
            tx_last_used: 0,
            tx_in_flight: false,
        };
        // rx/txの各queueを登録する。bufferはdesc 1本ずつで足りる。
        let region_ref: &NetQueueRegion = &device.region;
        let rx_len = virtio::setup_queue(
            &mut device.mmio,
            RX_QUEUE,
            region_ref.rx.desc.as_ptr(),
            &region_ref.rx.avail,
            &region_ref.rx.used,
            1,
        )
        .ok_or(NetError::QueueTooSmall)?;
        let tx_len = virtio::setup_queue(
            &mut device.mmio,
            TX_QUEUE,
            region_ref.tx.desc.as_ptr(),
            &region_ref.tx.avail,
            &region_ref.tx.used,
            1,
        )
        .ok_or(NetError::QueueTooSmall)?;
        device.queue_len = rx_len.min(tx_len);
        device.post_all_rx_buffers();
        device.mmio.write32(
            reg::STATUS,
            STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );
        Ok(device)
    }

    /// negotiation済みのdevice MAC address。
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// `net-arp`検査の診断用read-back。rxのavail/used idx、txのused idx、
    /// queue長、INTERRUPT_STATUSを返す。frameが届かないとき、queueが
    /// 動かなかったのかparseが落としたのかを切り分ける。
    #[cfg(feature = "qemu-test-net-arp")]
    pub fn debug_state(&self) -> (u16, u16, u16, u16, u32) {
        (
            unsafe { core::ptr::read_volatile(&raw const self.region.rx.avail.idx) },
            unsafe { core::ptr::read_volatile(&raw const self.region.rx.used.idx) },
            unsafe { core::ptr::read_volatile(&raw const self.region.tx.used.idx) },
            self.queue_len,
            self.mmio.read32(reg::INTERRUPT_STATUS),
        )
    }

    /// desc `index`へ対応するrx bufferを割り当てる。descとbufferの対応は
    /// 不変で、drain後の再postも同じindexを使う。
    fn set_rx_desc(&mut self, index: usize) {
        let buf = &mut self.rx_pages[index / RX_BUFS_PER_PAGE].bufs[index % RX_BUFS_PER_PAGE];
        self.region.rx.desc[index] = Desc {
            addr: buf.as_mut_ptr() as u64,
            len: RX_BUF_LEN as u32,
            flags: DESC_WRITE,
            next: 0,
        };
    }

    /// 初期post。queue長までのbufferをavail ringへ並べてnotifyする。
    fn post_all_rx_buffers(&mut self) {
        let posted = RX_BUFS.min(self.queue_len as usize);
        for index in 0..posted {
            self.set_rx_desc(index);
            self.region.rx.avail.ring[index] = index as u16;
        }
        // desc/buffer更新をavail idx公開の前にdeviceへ見せる。
        fence(Ordering::Release);
        self.rx_avail_idx = posted as u16;
        unsafe {
            core::ptr::write_volatile(&raw mut self.region.rx.avail.idx, self.rx_avail_idx);
        }
        // MMIO notifyがavail更新より先にdeviceへ届かないようにする。
        fence(Ordering::SeqCst);
        self.mmio.write32(reg::QUEUE_NOTIFY, RX_QUEUE);
    }

    /// rxのused ringから完了分を回収し、各frame (virtio_net_hdrを除く
    /// ethernet frame) を`sink`へ渡してからdescをdeviceへ再postする。
    /// 戻り値は消費したframe数。
    pub fn drain_rx(&mut self, sink: &mut impl FnMut(&[u8])) -> Result<usize, NetError> {
        let mut drained = 0usize;
        loop {
            let used_idx = unsafe { core::ptr::read_volatile(&raw const self.region.rx.used.idx) };
            if used_idx == self.rx_last_used {
                break;
            }
            // deviceのbuffer書き込みを読む前に完了を確定する。
            fence(Ordering::Acquire);
            let elem = unsafe {
                core::ptr::read_volatile(
                    &raw const self.region.rx.used.elems
                        [(self.rx_last_used % self.queue_len) as usize],
                )
            };
            self.rx_last_used = self.rx_last_used.wrapping_add(1);
            let id = elem.id as usize;
            if id >= RX_BUFS {
                return Err(NetError::BadUsedId(elem.id));
            }
            // used lenはvirtio_net_hdrを含むため、frame部分はhdr以降。
            let frame_len = (elem.len as usize)
                .saturating_sub(NET_HDR_LEN)
                .min(ETH_FRAME_MAX);
            {
                let buf = &self.rx_pages[id / RX_BUFS_PER_PAGE].bufs[id % RX_BUFS_PER_PAGE];
                sink(&buf[NET_HDR_LEN..NET_HDR_LEN + frame_len]);
            }
            // 同じdescへbufferを返す。descの内容は不変のためavailへの
            // 再登録だけでよい。
            self.region.rx.avail.ring[(self.rx_avail_idx % self.queue_len) as usize] =
                elem.id as u16;
            self.rx_avail_idx = self.rx_avail_idx.wrapping_add(1);
            drained += 1;
        }
        if drained > 0 {
            fence(Ordering::Release);
            unsafe {
                core::ptr::write_volatile(&raw mut self.region.rx.avail.idx, self.rx_avail_idx);
            }
            fence(Ordering::SeqCst);
            self.mmio.write32(reg::QUEUE_NOTIFY, RX_QUEUE);
        }
        Ok(drained)
    }

    /// txのusedが進んでいれば`tx_in_flight`を下ろす。used idは常に
    /// desc headの0を期待する。
    fn try_drain_tx(&mut self) -> Result<bool, NetError> {
        let used_idx = unsafe { core::ptr::read_volatile(&raw const self.region.tx.used.idx) };
        if used_idx == self.tx_last_used {
            return Ok(false);
        }
        fence(Ordering::Acquire);
        let elem = unsafe {
            core::ptr::read_volatile(
                &raw const self.region.tx.used.elems[(self.tx_last_used % self.queue_len) as usize],
            )
        };
        self.tx_last_used = self.tx_last_used.wrapping_add(1);
        self.tx_in_flight = false;
        if elem.id != 0 {
            return Err(NetError::BadUsedId(elem.id));
        }
        Ok(true)
    }

    /// 前のtx完了をboundedに待つ。`POLL_LIMIT`を超えたら`Timeout`を返し、
    /// `tx_in_flight`は残す（次回submitが改めて完了を拾う）。
    fn wait_tx_complete(&mut self) -> Result<(), NetError> {
        let mut spins = 0u32;
        while self.tx_in_flight {
            match self.try_drain_tx() {
                Ok(true) => {}
                Ok(false) => {
                    spins += 1;
                    if spins > POLL_LIMIT {
                        return Err(NetError::Timeout);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// ethernet frameをvirtio_net_hdr付きでtx queueへ流し、used完了を
    /// boundedに待つ。frameが`ETH_FRAME_MAX`を超える場合は送らない。
    pub fn transmit(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if frame.len() > ETH_FRAME_MAX {
            return Err(NetError::FrameTooLarge);
        }
        self.wait_tx_complete()?;
        self.region.tx_hdr.fill(0);
        self.region.tx_frame[..frame.len()].copy_from_slice(frame);
        // tx_hdrとtx_frameはregion内で隣接するため、1本のdescでhdr付き
        // packetをdeviceへ渡せる。
        let addr = core::ptr::addr_of!(self.region.tx_hdr) as u64;
        self.region.tx.desc[0] = Desc {
            addr,
            len: (NET_HDR_LEN + frame.len()) as u32,
            flags: 0,
            next: 0,
        };
        // desc/buffer更新をavail idx公開の前にdeviceへ見せる。
        fence(Ordering::Release);
        self.region.tx.avail.ring[(self.tx_avail_idx % self.queue_len) as usize] = 0;
        self.tx_avail_idx = self.tx_avail_idx.wrapping_add(1);
        unsafe {
            core::ptr::write_volatile(&raw mut self.region.tx.avail.idx, self.tx_avail_idx);
        }
        // MMIO notifyがavail更新より先にdeviceへ届かないようにする。
        fence(Ordering::SeqCst);
        self.mmio.write32(reg::QUEUE_NOTIFY, TX_QUEUE);
        self.tx_in_flight = true;
        self.wait_tx_complete()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::virtio::{MMIO_MAGIC, MMIO_VERSION, QUEUE_LEN, UsedElem, UsedRing};
    use std::alloc::{Layout, alloc_zeroed, dealloc};
    use std::vec::Vec;

    /// 整列済みregionをheap上に確保する所有権型。`NetQueueRegion`は
    /// `align(4096)`のため`Box`の整列保証 (16) では不足する。
    struct TestQueueRegion {
        ptr: *mut NetQueueRegion,
    }

    impl TestQueueRegion {
        fn new() -> Self {
            let layout = Layout::from_size_align(4096, 4096).expect("page layout is valid");
            // Safety: layoutは有効。使用は`Drop`までの`DerefMut`経由のみ。
            let ptr = unsafe { alloc_zeroed(layout) }.cast::<NetQueueRegion>();
            assert!(!ptr.is_null());
            Self { ptr }
        }
    }

    impl core::ops::Deref for TestQueueRegion {
        type Target = NetQueueRegion;
        fn deref(&self) -> &Self::Target {
            unsafe { &*self.ptr }
        }
    }

    impl core::ops::DerefMut for TestQueueRegion {
        fn deref_mut(&mut self) -> &mut Self::Target {
            unsafe { &mut *self.ptr }
        }
    }

    impl Drop for TestQueueRegion {
        fn drop(&mut self) {
            let layout = Layout::from_size_align(4096, 4096).expect("page layout is valid");
            unsafe { dealloc(self.ptr.cast::<u8>(), layout) };
        }
    }

    struct TestRxPage {
        ptr: *mut NetRxBufPage,
    }

    impl TestRxPage {
        fn new() -> Self {
            let layout = Layout::from_size_align(4096, 4096).expect("page layout is valid");
            let ptr = unsafe { alloc_zeroed(layout) }.cast::<NetRxBufPage>();
            assert!(!ptr.is_null());
            Self { ptr }
        }
    }

    impl core::ops::Deref for TestRxPage {
        type Target = NetRxBufPage;
        fn deref(&self) -> &Self::Target {
            unsafe { &*self.ptr }
        }
    }

    impl core::ops::DerefMut for TestRxPage {
        fn deref_mut(&mut self) -> &mut Self::Target {
            unsafe { &mut *self.ptr }
        }
    }

    impl Drop for TestRxPage {
        fn drop(&mut self) {
            let layout = Layout::from_size_align(4096, 4096).expect("page layout is valid");
            unsafe { dealloc(self.ptr.cast::<u8>(), layout) };
        }
    }

    /// virtio-netの最小device応答を真似るmock。txのQUEUE_NOTIFYでは
    /// used entryを進め、rxは`inject_rx`でbuffer書き込み+used登録する。
    struct FakeNet {
        regs: [u32; 64],
        config: [u8; 64],
        magic: u32,
        version: u32,
        device_id: u32,
        features_lo: u32,
        features_hi: u32,
        queue_num_max: u32,
        accept_features_ok: bool,
        device_features_sel: u32,
        // queue indexごとのdesc/driver/device領域アドレス。
        desc_addr: [usize; 2],
        driver_addr: [usize; 2],
        device_addr: [usize; 2],
        queue_sel: usize,
        used_id: u32,
        never_complete: bool,
    }

    impl FakeNet {
        fn new() -> Self {
            let mut device = Self {
                regs: [0; 64],
                config: [0; 64],
                magic: MMIO_MAGIC,
                version: MMIO_VERSION,
                device_id: DEVICE_ID_NET,
                features_lo: FEATURE_MAC_LO | FEATURE_MRG_RXBUF_LO,
                features_hi: FEATURE_VERSION_1_HI,
                queue_num_max: QUEUE_LEN as u32,
                accept_features_ok: true,
                device_features_sel: 0,
                desc_addr: [0; 2],
                driver_addr: [0; 2],
                device_addr: [0; 2],
                queue_sel: 0,
                used_id: 0,
                never_complete: false,
            };
            // QEMUの既定guest MACに相当する値をconfig領域へ置く。
            device.config[..6].copy_from_slice(&[0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
            device
        }

        /// tx queueのavailを1件消費し、virtio_net_hdrとframe長を検査して
        /// usedへ完了を書く。
        fn complete_tx(&mut self) {
            if self.never_complete {
                return;
            }
            unsafe {
                let used = &mut *(self.device_addr[1] as *mut UsedRing);
                let descs = &*(self.desc_addr[1] as *const [Desc; QUEUE_LEN]);
                assert_eq!(descs[0].flags, 0);
                assert!(descs[0].len as usize >= NET_HDR_LEN);
                // virtio_net_hdrは10 byteすべて0を要求する。
                let hdr = &*(descs[0].addr as *const [u8; NET_HDR_LEN]);
                assert_eq!(hdr, &[0; NET_HDR_LEN]);

                let slot = (used.idx % QUEUE_LEN as u16) as usize;
                used.elems[slot] = UsedElem {
                    id: self.used_id,
                    len: descs[0].len,
                };
                used.idx = used.idx.wrapping_add(1);
                self.regs[reg::INTERRUPT_STATUS / 4] = 1;
            }
        }

        /// rx queueのposted buffer `id`へ`frame`を書き込み、usedへ
        /// 完了を登録する。
        fn inject_rx(&mut self, id: u32, frame: &[u8]) {
            unsafe {
                let descs = &*(self.desc_addr[0] as *const [Desc; QUEUE_LEN]);
                assert_eq!(descs[id as usize].flags, DESC_WRITE);
                let buf = descs[id as usize].addr as *mut u8;
                buf.write_bytes(0, NET_HDR_LEN);
                core::ptr::copy_nonoverlapping(frame.as_ptr(), buf.add(NET_HDR_LEN), frame.len());

                let used = &mut *(self.device_addr[0] as *mut UsedRing);
                let slot = (used.idx % QUEUE_LEN as u16) as usize;
                used.elems[slot] = UsedElem {
                    id,
                    len: (NET_HDR_LEN + frame.len()) as u32,
                };
                used.idx = used.idx.wrapping_add(1);
                self.regs[reg::INTERRUPT_STATUS / 4] = 1;
            }
        }
    }

    impl Mmio for FakeNet {
        fn read32(&self, offset: usize) -> u32 {
            match offset {
                reg::MAGIC => self.magic,
                reg::VERSION => self.version,
                reg::DEVICE_ID => self.device_id,
                reg::DEVICE_FEATURES => {
                    if self.device_features_sel == 0 {
                        self.features_lo
                    } else {
                        self.features_hi
                    }
                }
                reg::QUEUE_NUM_MAX => self.queue_num_max,
                reg::STATUS => self.regs[reg::STATUS / 4],
                reg::INTERRUPT_STATUS => self.regs[reg::INTERRUPT_STATUS / 4],
                o if o >= reg::CONFIG => u32::from_le_bytes(
                    self.config[o - reg::CONFIG..o - reg::CONFIG + 4]
                        .try_into()
                        .expect("config word"),
                ),
                _ => 0,
            }
        }

        fn write32(&mut self, offset: usize, value: u32) {
            match offset {
                reg::STATUS => {
                    let mut stored = value;
                    if value & STATUS_FEATURES_OK != 0 && !self.accept_features_ok {
                        stored &= !STATUS_FEATURES_OK;
                    }
                    self.regs[reg::STATUS / 4] = stored;
                }
                reg::DEVICE_FEATURES_SEL => self.device_features_sel = value,
                reg::DRIVER_FEATURES_SEL => {}
                reg::DRIVER_FEATURES => {}
                reg::QUEUE_SEL => self.queue_sel = value as usize,
                reg::QUEUE_DESC_LO => {
                    self.desc_addr[self.queue_sel] =
                        (self.desc_addr[self.queue_sel] & !0xffff_ffff) | value as usize;
                }
                reg::QUEUE_DRIVER_LO => {
                    self.driver_addr[self.queue_sel] =
                        (self.driver_addr[self.queue_sel] & !0xffff_ffff) | value as usize;
                }
                reg::QUEUE_DEVICE_LO => {
                    self.device_addr[self.queue_sel] =
                        (self.device_addr[self.queue_sel] & !0xffff_ffff) | value as usize;
                }
                reg::QUEUE_DESC_HI => {
                    self.desc_addr[self.queue_sel] =
                        (self.desc_addr[self.queue_sel] & 0xffff_ffff) | (value as usize) << 32;
                }
                reg::QUEUE_DRIVER_HI => {
                    self.driver_addr[self.queue_sel] =
                        (self.driver_addr[self.queue_sel] & 0xffff_ffff) | (value as usize) << 32;
                }
                reg::QUEUE_DEVICE_HI => {
                    self.device_addr[self.queue_sel] =
                        (self.device_addr[self.queue_sel] & 0xffff_ffff) | (value as usize) << 32;
                }
                reg::QUEUE_NUM => {}
                reg::QUEUE_READY => {}
                reg::INTERRUPT_ACK => self.regs[reg::INTERRUPT_STATUS / 4] &= !value,
                reg::QUEUE_NOTIFY if value == TX_QUEUE => self.complete_tx(),
                reg::QUEUE_NOTIFY => {}
                _ => {}
            }
        }
    }

    fn device_and_region() -> (FakeNet, TestQueueRegion, [TestRxPage; RX_BUF_PAGES]) {
        (
            FakeNet::new(),
            TestQueueRegion::new(),
            core::array::from_fn(|_| TestRxPage::new()),
        )
    }

    // Catches a skipped handshake step, the MAC not being read from config,
    // or the rx buffers not being posted before DRIVER_OK.
    #[test]
    fn init_negotiates_features_reads_mac_and_posts_rx_buffers() {
        let (device, region, rx_pages) = device_and_region();
        let net = VirtioNet::init(device, region, rx_pages).unwrap();
        assert_eq!(net.mac(), [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let avail_idx = unsafe { core::ptr::read_volatile(&raw const net.region.rx.avail.idx) };
        assert_eq!(avail_idx, RX_BUFS as u16);
        // 全descがdevice-writableで対応bufferを指している。
        for index in 0..RX_BUFS {
            assert_eq!(net.region.rx.desc[index].flags, DESC_WRITE);
            let expected =
                net.rx_pages[index / RX_BUFS_PER_PAGE].bufs[index % RX_BUFS_PER_PAGE].as_ptr();
            assert_eq!(net.region.rx.desc[index].addr, expected as u64);
        }
    }

    // Catches accepting a device with a corrupt register file or no MAC.
    #[test]
    fn init_rejects_bad_magic_non_net_devices_and_missing_mac() {
        let (mut device, region, rx_pages) = device_and_region();
        device.magic = 0;
        assert_eq!(
            VirtioNet::init(device, region, rx_pages).err().unwrap(),
            NetError::BadMagic
        );

        let (mut device, region, rx_pages) = device_and_region();
        device.device_id = 2; // block
        assert_eq!(
            VirtioNet::init(device, region, rx_pages).err().unwrap(),
            NetError::NotNetDevice(2)
        );

        let (mut device, region, rx_pages) = device_and_region();
        device.features_lo = 0;
        assert_eq!(
            VirtioNet::init(device, region, rx_pages).err().unwrap(),
            NetError::NoMac
        );
    }

    // Catches transmit skipping the header, the avail push, or the used wait.
    #[test]
    fn transmit_sends_header_and_frame_then_drains_used() {
        let (device, region, rx_pages) = device_and_region();
        let mut net = VirtioNet::init(device, region, rx_pages).unwrap();
        let frame = [0xabu8; 60];
        net.transmit(&frame).unwrap();
        // desc0がhdr+frameを差し、deviceがusedを進めたこと。
        assert_eq!(net.region.tx.desc[0].len, (NET_HDR_LEN + 60) as u32);
        assert_eq!(&net.region.tx_frame[..60], &frame);
        assert!(!net.tx_in_flight);
        let used_idx = unsafe { core::ptr::read_volatile(&raw const net.region.tx.used.idx) };
        assert_eq!(used_idx, 1);
    }

    // Catches oversized frames reaching the device or missing poll bounds.
    #[test]
    fn transmit_bounds_frame_size_and_times_out() {
        let (device, region, rx_pages) = device_and_region();
        let mut net = VirtioNet::init(device, region, rx_pages).unwrap();
        let big = [0u8; ETH_FRAME_MAX + 1];
        assert_eq!(net.transmit(&big), Err(NetError::FrameTooLarge));

        let (mut device, region, rx_pages) = device_and_region();
        device.never_complete = true;
        let mut net = VirtioNet::init(device, region, rx_pages).unwrap();
        let frame = [1u8; 32];
        assert_eq!(net.transmit(&frame), Err(NetError::Timeout));
        assert!(net.tx_in_flight);
        // 遅れた完了を次回submitが拾う。
        net.mmio.never_complete = false;
        net.mmio.complete_tx();
        assert_eq!(net.transmit(&frame), Ok(()));
    }

    // Catches drain losing a frame, failing to repost the buffer, or
    // passing the virtio_net_hdr to the sink.
    #[test]
    fn drain_rx_delivers_frames_and_reposts_buffers() {
        let (mut device, region, rx_pages) = device_and_region();
        let mut net = VirtioNet::init(device, region, rx_pages).unwrap();
        let frame0 = [0x11u8; 64];
        let frame1 = [0x22u8; 128];
        net.mmio.inject_rx(3, &frame0);
        net.mmio.inject_rx(5, &frame1);

        let mut seen: Vec<Vec<u8>> = Vec::new();
        let drained = net
            .drain_rx(&mut |frame| seen.push(frame.to_vec()))
            .unwrap();
        assert_eq!(drained, 2);
        assert_eq!(seen[0], frame0.to_vec());
        assert_eq!(seen[1], frame1.to_vec());
        // 2件分がavailへ再postされたこと。
        let avail_idx = unsafe { core::ptr::read_volatile(&raw const net.region.rx.avail.idx) };
        assert_eq!(avail_idx, RX_BUFS as u16 + 2);
        // 完了の無いときは0を返す。
        assert_eq!(net.drain_rx(&mut |_| {}).unwrap(), 0);
    }

    // Catches accepting a corrupt used id instead of rejecting it.
    #[test]
    fn drain_rx_rejects_an_out_of_range_used_id() {
        let (mut device, region, rx_pages) = device_and_region();
        let mut net = VirtioNet::init(device, region, rx_pages).unwrap();
        unsafe {
            let used = &mut *(net.mmio.device_addr[0] as *mut UsedRing);
            used.elems[0] = UsedElem { id: 99, len: 32 };
            used.idx = 1;
        }
        assert_eq!(net.drain_rx(&mut |_| {}), Err(NetError::BadUsedId(99)));
    }
}
