//! virtio-mmio register fileへのvolatileアクセス。ベースアドレスはFDTが
//! 報告したMMIO領域であり、`KernelMapPlan::with_device_pages`でS-mode
//! R+Wとしてmap済みであることが前提。

use minios_kernel::virtio::Mmio;

pub struct MmioRegs {
    base: usize,
}

impl MmioRegs {
    /// # Safety
    /// `base`はvirtio-mmio register fileの先頭であり、現在のaddress spaceで
    /// R+Wにmapされていること。
    pub const unsafe fn new(base: usize) -> Self {
        Self { base }
    }
}

impl Mmio for MmioRegs {
    fn read32(&self, offset: usize) -> u32 {
        unsafe { ((self.base + offset) as *const u32).read_volatile() }
    }

    fn write32(&mut self, offset: usize, value: u32) {
        unsafe { ((self.base + offset) as *mut u32).write_volatile(value) }
    }
}

/// source番号に対応するvirtio deviceのinterrupt lineをackする。
/// PLICのclaimハンドラが使う。範囲外のsourceでは何もしない。
#[cfg(target_arch = "riscv64")]
pub fn ack_irq_source(source: u32) {
    let spec = crate::machine::spec();
    let Some(idx) = spec.virtio_mmio_irq[..spec.virtio_mmio_count]
        .iter()
        .position(|&irq| irq == source)
    else {
        return;
    };
    let base = spec.virtio_mmio[idx];
    // Safety: `base`はFDTが報告し`with_device_pages`でmap済みのMMIO領域。
    let mut regs = unsafe { MmioRegs::new(base) };
    regs.interrupt_ack();
}
