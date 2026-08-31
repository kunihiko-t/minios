#[cfg(any(target_arch = "riscv64", test))]
use crate::memory::frame::PAGE_SIZE;

pub trait FrameStore {
    type Error;

    fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error>;
    fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error>;
    fn write_u64(
        &mut self,
        frame_start: usize,
        index: usize,
        value: u64,
    ) -> Result<(), Self::Error>;
    fn copy_into(
        &mut self,
        frame_start: usize,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), Self::Error>;
    fn copy_out(
        &self,
        frame_start: usize,
        offset: usize,
        output: &mut [u8],
    ) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityFrameStoreError {
    UnalignedManagedRange,
    InvalidManagedRange,
    UnalignedFrame,
    AddressOverflow,
    FrameOutsideManagedRange,
    IndexOutOfBounds,
    RangeOutOfBounds,
    BufferOverlapsCopyRange,
}

#[cfg(any(target_arch = "riscv64", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IdentityFrameRange {
    start: usize,
    end: usize,
}

#[cfg(any(target_arch = "riscv64", test))]
impl IdentityFrameRange {
    fn try_new(start: usize, end: usize) -> Result<Self, IdentityFrameStoreError> {
        if !start.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) {
            return Err(IdentityFrameStoreError::UnalignedManagedRange);
        }
        if start >= end {
            return Err(IdentityFrameStoreError::InvalidManagedRange);
        }
        Ok(Self { start, end })
    }
}

#[cfg(any(target_arch = "riscv64", test))]
fn checked_frame(
    managed: &IdentityFrameRange,
    frame_start: usize,
) -> Result<(), IdentityFrameStoreError> {
    if !frame_start.is_multiple_of(PAGE_SIZE) {
        return Err(IdentityFrameStoreError::UnalignedFrame);
    }
    let frame_end = frame_start
        .checked_add(PAGE_SIZE)
        .ok_or(IdentityFrameStoreError::AddressOverflow)?;
    if frame_start < managed.start || frame_end > managed.end {
        return Err(IdentityFrameStoreError::FrameOutsideManagedRange);
    }
    Ok(())
}

#[cfg(any(target_arch = "riscv64", test))]
fn checked_word_offset(
    managed: &IdentityFrameRange,
    frame_start: usize,
    index: usize,
) -> Result<usize, IdentityFrameStoreError> {
    checked_frame(managed, frame_start)?;
    if index >= PAGE_SIZE / size_of::<u64>() {
        return Err(IdentityFrameStoreError::IndexOutOfBounds);
    }
    Ok(index * size_of::<u64>())
}

#[cfg(any(target_arch = "riscv64", test))]
fn checked_frame_range(
    managed: &IdentityFrameRange,
    frame_start: usize,
    offset: usize,
    len: usize,
) -> Result<usize, IdentityFrameStoreError> {
    checked_frame(managed, frame_start)?;
    let end = offset
        .checked_add(len)
        .ok_or(IdentityFrameStoreError::RangeOutOfBounds)?;
    if end > PAGE_SIZE {
        return Err(IdentityFrameStoreError::RangeOutOfBounds);
    }
    Ok(offset)
}

#[cfg(any(target_arch = "riscv64", test))]
fn checked_copy_buffer(
    copy_start: usize,
    copy_len: usize,
    buffer_start: usize,
    buffer_len: usize,
) -> Result<(), IdentityFrameStoreError> {
    let copy_end = copy_start
        .checked_add(copy_len)
        .ok_or(IdentityFrameStoreError::AddressOverflow)?;
    let buffer_end = buffer_start
        .checked_add(buffer_len)
        .ok_or(IdentityFrameStoreError::AddressOverflow)?;
    if copy_len != 0 && buffer_len != 0 && buffer_start < copy_end && buffer_end > copy_start {
        return Err(IdentityFrameStoreError::BufferOverlapsCopyRange);
    }
    Ok(())
}

#[cfg(target_arch = "riscv64")]
#[derive(Debug)]
pub struct IdentityFrameStore {
    managed: IdentityFrameRange,
}

#[cfg(target_arch = "riscv64")]
impl IdentityFrameStore {
    /// Creates a checked byte-access capability for an identity-mapped RAM range.
    ///
    /// # Safety
    ///
    /// If this function returns `Ok`, `managed_start..managed_end` must remain
    /// valid RAM and identity-mapped for the entire lifetime of the returned
    /// store. No overlapping `IdentityFrameStore` may exist. For each operation,
    /// the caller must own the target frame and prevent concurrent access to the
    /// same bytes. Allocator-owned frames may back kernel Rust buffers (for
    /// example a trap stack); copy operations check that such a buffer is
    /// disjoint from their actual source/destination range before raw access.
    /// If validation returns `Err`, no capability is created.
    pub unsafe fn new(
        managed_start: usize,
        managed_end: usize,
    ) -> Result<Self, IdentityFrameStoreError> {
        Ok(Self {
            managed: IdentityFrameRange::try_new(managed_start, managed_end)?,
        })
    }
}

#[cfg(target_arch = "riscv64")]
impl FrameStore for IdentityFrameStore {
    type Error = IdentityFrameStoreError;

    fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error> {
        checked_frame_range(&self.managed, frame_start, 0, PAGE_SIZE)?;
        // Safety: on the RISC-V kernel target, allocator-owned physical RAM is
        // available through the S-mode identity map. The full page range was
        // checked before deriving the pointer, and the caller's exclusive
        // frame ownership prevents concurrent Rust access.
        unsafe { core::ptr::write_bytes(frame_start as *mut u8, 0, PAGE_SIZE) };
        Ok(())
    }

    fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
        let offset = checked_word_offset(&self.managed, frame_start, index)?;
        let address = frame_start
            .checked_add(offset)
            .ok_or(IdentityFrameStoreError::AddressOverflow)?;
        // Safety: the checked word lies entirely within the aligned identity-
        // mapped frame. Page-table words are naturally aligned u64 values.
        Ok(unsafe { core::ptr::read_volatile(address as *const u64) })
    }

    fn write_u64(
        &mut self,
        frame_start: usize,
        index: usize,
        value: u64,
    ) -> Result<(), Self::Error> {
        let offset = checked_word_offset(&self.managed, frame_start, index)?;
        let address = frame_start
            .checked_add(offset)
            .ok_or(IdentityFrameStoreError::AddressOverflow)?;
        // Safety: the checked word lies entirely within the aligned identity-
        // mapped frame, and the mutable store borrow serializes writes.
        unsafe { core::ptr::write_volatile(address as *mut u64, value) };
        Ok(())
    }

    fn copy_into(
        &mut self,
        frame_start: usize,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        let offset = checked_frame_range(&self.managed, frame_start, offset, bytes.len())?;
        let destination = frame_start
            .checked_add(offset)
            .ok_or(IdentityFrameStoreError::AddressOverflow)?;
        checked_copy_buffer(
            destination,
            bytes.len(),
            bytes.as_ptr() as usize,
            bytes.len(),
        )?;
        // Safety: the destination frame and byte range were checked before
        // pointer derivation. The caller's target-frame ownership serializes
        // access to these bytes, and the runtime check rejects an overlapping
        // source buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), destination as *mut u8, bytes.len())
        };
        Ok(())
    }

    fn copy_out(
        &self,
        frame_start: usize,
        offset: usize,
        output: &mut [u8],
    ) -> Result<(), Self::Error> {
        let offset = checked_frame_range(&self.managed, frame_start, offset, output.len())?;
        let source = frame_start
            .checked_add(offset)
            .ok_or(IdentityFrameStoreError::AddressOverflow)?;
        checked_copy_buffer(
            source,
            output.len(),
            output.as_mut_ptr() as usize,
            output.len(),
        )?;
        // Safety: the source frame and byte range were checked before pointer
        // derivation. Target-frame ownership serializes reads from the source,
        // and the runtime check rejects an overlapping output buffer while
        // allowing a disjoint allocator-owned kernel buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(source as *const u8, output.as_mut_ptr(), output.len())
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IdentityFrameRange, IdentityFrameStoreError, checked_copy_buffer, checked_frame,
        checked_frame_range, checked_word_offset,
    };
    use crate::memory::frame::PAGE_SIZE;

    fn managed_range() -> IdentityFrameRange {
        IdentityFrameRange::try_new(0x4000, 0x8000).unwrap()
    }

    // Catches accepting an empty, reversed, or unaligned direct-map capability
    // before an IdentityFrameStore can retain it.
    #[test]
    fn identity_store_range_requires_aligned_nonempty_bounds() {
        assert_eq!(
            IdentityFrameRange::try_new(0x4001, 0x8000),
            Err(IdentityFrameStoreError::UnalignedManagedRange)
        );
        assert_eq!(
            IdentityFrameRange::try_new(0x4000, 0x4000),
            Err(IdentityFrameStoreError::InvalidManagedRange)
        );
        assert_eq!(
            IdentityFrameRange::try_new(0x8000, 0x4000),
            Err(IdentityFrameStoreError::InvalidManagedRange)
        );
    }

    // Catches arbitrary aligned addresses, end-inclusive bounds, or wrapping
    // frame-end arithmetic reaching raw-pointer derivation.
    #[test]
    fn identity_store_accepts_only_complete_frames_inside_the_managed_range() {
        let managed = managed_range();
        let last_aligned_address = usize::MAX & !(PAGE_SIZE - 1);

        assert_eq!(
            checked_frame(&managed, 0),
            Err(IdentityFrameStoreError::FrameOutsideManagedRange)
        );
        assert_eq!(
            checked_frame(&managed, 0x3000),
            Err(IdentityFrameStoreError::FrameOutsideManagedRange)
        );
        assert_eq!(checked_frame(&managed, 0x7000), Ok(()));
        assert_eq!(
            checked_frame(&managed, 0x8000),
            Err(IdentityFrameStoreError::FrameOutsideManagedRange)
        );
        assert_eq!(
            checked_frame(&managed, last_aligned_address),
            Err(IdentityFrameStoreError::AddressOverflow)
        );
    }

    // Catches deriving a pointer from an unaligned frame base or accepting the
    // first u64 index beyond a 4 KiB page table frame.
    #[test]
    fn identity_store_rejects_invalid_frame_and_word_boundaries() {
        let managed = managed_range();
        assert_eq!(
            checked_word_offset(&managed, 0x4001, 0),
            Err(IdentityFrameStoreError::UnalignedFrame)
        );
        assert_eq!(checked_word_offset(&managed, 0x4000, 511), Ok(4088));
        assert_eq!(
            checked_word_offset(&managed, 0x4000, 512),
            Err(IdentityFrameStoreError::IndexOutOfBounds)
        );
    }

    // Catches offset+length overflow and ranges crossing the final byte of a
    // frame before copy_into/copy_out derive a raw pointer.
    #[test]
    fn identity_store_rejects_byte_ranges_outside_one_frame() {
        let managed = managed_range();
        assert_eq!(checked_frame_range(&managed, 0x7000, 4095, 1), Ok(4095));
        assert_eq!(checked_frame_range(&managed, 0x7000, 4096, 0), Ok(4096));
        assert_eq!(
            checked_frame_range(&managed, 0x7000, 4095, 2),
            Err(IdentityFrameStoreError::RangeOutOfBounds)
        );
        assert_eq!(
            checked_frame_range(&managed, 0x7000, usize::MAX, 2),
            Err(IdentityFrameStoreError::RangeOutOfBounds)
        );
    }

    // Catches rejecting a kernel buffer in another allocator-owned frame, or
    // accepting a buffer that overlaps the actual copy source/destination.
    #[test]
    fn identity_store_allows_disjoint_managed_buffers_and_rejects_copy_overlap() {
        assert_eq!(checked_copy_buffer(0x4000, 0x1000, 0x6000, 0x1000), Ok(()));
        assert_eq!(checked_copy_buffer(0x4000, 0x1000, 0x3000, 0x1000), Ok(()));
        assert_eq!(checked_copy_buffer(0x4000, 0x1000, 0x5000, 1), Ok(()));
        assert_eq!(checked_copy_buffer(0x4000, 0, 0x4000, 1), Ok(()));
        assert_eq!(
            checked_copy_buffer(0x4000, 0x1000, 0x4000, 1),
            Err(IdentityFrameStoreError::BufferOverlapsCopyRange)
        );
        assert_eq!(
            checked_copy_buffer(0x4000, 0x1000, usize::MAX, 2),
            Err(IdentityFrameStoreError::AddressOverflow)
        );
    }
}
