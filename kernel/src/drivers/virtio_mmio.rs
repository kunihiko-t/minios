//! virtio-mmio register fileへのvolatileアクセス。ベースアドレスはFDTが
//! 報告したMMIO領域であり、`KernelMapPlan::with_device_pages`でS-mode
//! R+Wとしてmap済みであることが前提。

use minios_kernel::storage::virtio_blk::Mmio;

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
