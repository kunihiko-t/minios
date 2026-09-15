pub mod frame;
pub mod heap;

// この節の定数はQEMU `virt` `-m 128M`の参照レイアウトである。
// 実行時はDTBから組み立てた`fdt::MachineSpec`が同じ値を導き、
// kernelはspec側の値で動く。こちらはホストテストと文書の期待値として残す。
pub const PHYSICAL_MEMORY_END: usize = 0x8780_0000;
pub const BOOT_PAYLOAD_START: usize = 0x8780_0000;
/// QEMUがDTBを置く最終2 MiBを残した、payload窓の排他的な上端。
pub const BOOT_PAYLOAD_END: usize = 0x87e0_0000;
/// QEMU `virt`がFDTを書き込む`0x87e0_0000..0x8800_0000`の予約領域。
pub const FDT_RESERVED_START: usize = 0x87e0_0000;
pub const FDT_RESERVED_END: usize = 0x8800_0000;
/// managed RAMの末尾へ切り出すヒープ初期領域の長さ。
/// `FrameAllocator`の管理上端はこの分だけ`managed_end`より下になる。
/// ヒープはOOM時にallocatorの最上位pageを`allocate_at`で取り込み、
/// この初期位置から下へ連続して成長する。
/// QEMU `virt` `-m 128M`では初期領域は`0x8770_0000..0x8780_0000`となる。
pub const KERNEL_HEAP_LEN: usize = 1024 * 1024;

#[allow(dead_code)]
pub struct KernelSections {
    text: core::ops::Range<usize>,
    rodata: core::ops::Range<usize>,
    writable: core::ops::Range<usize>,
    boot_stack: core::ops::Range<usize>,
    kernel_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    Unaligned,
    Empty,
    Overlap,
    EndMismatch,
}

impl KernelSections {
    pub fn new(
        text: core::ops::Range<usize>,
        rodata: core::ops::Range<usize>,
        writable: core::ops::Range<usize>,
        boot_stack: core::ops::Range<usize>,
        kernel_end: usize,
    ) -> Result<Self, LayoutError> {
        let boundaries = [
            text.start,
            text.end,
            rodata.start,
            rodata.end,
            writable.start,
            writable.end,
            boot_stack.start,
            boot_stack.end,
            kernel_end,
        ];
        if boundaries
            .iter()
            .any(|boundary| !boundary.is_multiple_of(frame::PAGE_SIZE))
        {
            return Err(LayoutError::Unaligned);
        }
        if text.start >= text.end
            || rodata.start >= rodata.end
            || writable.start >= writable.end
            || boot_stack.start >= boot_stack.end
        {
            return Err(LayoutError::Empty);
        }
        if text.end > rodata.start || rodata.end > writable.start || writable.end > boot_stack.start
        {
            return Err(LayoutError::Overlap);
        }
        if boot_stack.end != kernel_end {
            return Err(LayoutError::EndMismatch);
        }

        Ok(Self {
            text,
            rodata,
            writable,
            boot_stack,
            kernel_end,
        })
    }

    pub const fn kernel_end(&self) -> usize {
        self.kernel_end
    }

    pub(crate) fn text(&self) -> core::ops::Range<usize> {
        self.text.clone()
    }

    pub(crate) fn rodata(&self) -> core::ops::Range<usize> {
        self.rodata.clone()
    }

    pub(crate) fn writable(&self) -> core::ops::Range<usize> {
        self.writable.clone()
    }

    pub(crate) fn boot_stack(&self) -> core::ops::Range<usize> {
        self.boot_stack.clone()
    }
}
