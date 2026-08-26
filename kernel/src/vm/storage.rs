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
    BufferOverlapsManagedRange,
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
fn checked_external_buffer(
    managed: &IdentityFrameRange,
    buffer_start: usize,
    len: usize,
) -> Result<(), IdentityFrameStoreError> {
    let buffer_end = buffer_start
        .checked_add(len)
        .ok_or(IdentityFrameStoreError::AddressOverflow)?;
    if len != 0 && buffer_start < managed.end && buffer_end > managed.start {
        return Err(IdentityFrameStoreError::BufferOverlapsManagedRange);
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
    /// Creates the sole byte-access capability for an identity-mapped RAM range.
    ///
    /// # Safety
    ///
    /// If this function returns `Ok`, `managed_start..managed_end` must remain
    /// valid RAM and identity-mapped for the entire lifetime of the returned
    /// store. The caller must grant this store exclusive byte access to the
    /// complete range: no overlapping `IdentityFrameStore`, raw access, or Rust
    /// reference may read or write it except through this store. A frame
    /// allocator may assign ownership within the range, but it must not itself
    /// dereference frame memory. If validation returns `Err`, no capability is
    /// created and the caller retains all obligations.
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
        checked_external_buffer(&self.managed, bytes.as_ptr() as usize, bytes.len())?;
        // Safety: the destination frame and byte range were checked before
        // pointer derivation. The unsafe constructor grants the store exclusive
        // access to all managed bytes, and the runtime buffer check rejects an
        // overlapping source even if that constructor contract was violated.
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
        checked_external_buffer(&self.managed, output.as_ptr() as usize, output.len())?;
        // Safety: the source frame and byte range were checked before pointer
        // derivation. The unsafe constructor excludes Rust references into the
        // managed range, and the runtime check also rejects overlapping output.
        unsafe {
            core::ptr::copy_nonoverlapping(source as *const u8, output.as_mut_ptr(), output.len())
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IdentityFrameRange, IdentityFrameStoreError, checked_external_buffer, checked_frame,
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

    // Catches copy_into/copy_out accepting a Rust reference into the store's
    // exclusively held direct-map range, or wrapping its buffer end.
    #[test]
    fn identity_store_rejects_buffers_that_alias_managed_ram() {
        let managed = managed_range();

        assert_eq!(checked_external_buffer(&managed, 0x3000, 0x1000), Ok(()));
        assert_eq!(checked_external_buffer(&managed, 0x8000, 1), Ok(()));
        assert_eq!(checked_external_buffer(&managed, 0x5000, 0), Ok(()));
        assert_eq!(
            checked_external_buffer(&managed, 0x5000, 1),
            Err(IdentityFrameStoreError::BufferOverlapsManagedRange)
        );
        assert_eq!(
            checked_external_buffer(&managed, usize::MAX, 2),
            Err(IdentityFrameStoreError::AddressOverflow)
        );
    }
}
