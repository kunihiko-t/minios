pub mod frame;

pub const PHYSICAL_MEMORY_END: usize = 0x8780_0000;
pub const BOOT_PAYLOAD_START: usize = 0x8780_0000;
pub const BOOT_PAYLOAD_END: usize = 0x8800_0000;

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
}
