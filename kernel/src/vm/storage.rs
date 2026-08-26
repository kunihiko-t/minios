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
    UnalignedFrame,
    IndexOutOfBounds,
    RangeOutOfBounds,
}

#[cfg(any(target_arch = "riscv64", test))]
fn checked_frame_start(frame_start: usize) -> Result<(), IdentityFrameStoreError> {
    if !frame_start.is_multiple_of(PAGE_SIZE) {
        return Err(IdentityFrameStoreError::UnalignedFrame);
    }
    Ok(())
}

#[cfg(any(target_arch = "riscv64", test))]
fn checked_word_offset(frame_start: usize, index: usize) -> Result<usize, IdentityFrameStoreError> {
    checked_frame_start(frame_start)?;
    if index >= PAGE_SIZE / size_of::<u64>() {
        return Err(IdentityFrameStoreError::IndexOutOfBounds);
    }
    Ok(index * size_of::<u64>())
}

#[cfg(any(target_arch = "riscv64", test))]
fn checked_frame_range(
    frame_start: usize,
    offset: usize,
    len: usize,
) -> Result<usize, IdentityFrameStoreError> {
    checked_frame_start(frame_start)?;
    let end = offset
        .checked_add(len)
        .ok_or(IdentityFrameStoreError::RangeOutOfBounds)?;
    if end > PAGE_SIZE {
        return Err(IdentityFrameStoreError::RangeOutOfBounds);
    }
    Ok(offset)
}

#[cfg(target_arch = "riscv64")]
#[derive(Debug, Default)]
pub struct IdentityFrameStore;

#[cfg(target_arch = "riscv64")]
impl IdentityFrameStore {
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(target_arch = "riscv64")]
impl FrameStore for IdentityFrameStore {
    type Error = IdentityFrameStoreError;

    fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error> {
        checked_frame_range(frame_start, 0, PAGE_SIZE)?;
        // Safety: on the RISC-V kernel target, allocator-owned physical RAM is
        // available through the S-mode identity map. The full page range was
        // checked before deriving the pointer, and the caller's exclusive
        // frame ownership prevents concurrent Rust access.
        unsafe { core::ptr::write_bytes(frame_start as *mut u8, 0, PAGE_SIZE) };
        Ok(())
    }

    fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
        let offset = checked_word_offset(frame_start, index)?;
        // Safety: the checked word lies entirely within the aligned identity-
        // mapped frame. Page-table words are naturally aligned u64 values.
        Ok(unsafe { core::ptr::read_volatile((frame_start + offset) as *const u64) })
    }

    fn write_u64(
        &mut self,
        frame_start: usize,
        index: usize,
        value: u64,
    ) -> Result<(), Self::Error> {
        let offset = checked_word_offset(frame_start, index)?;
        // Safety: the checked word lies entirely within the aligned identity-
        // mapped frame, and the mutable store borrow serializes writes.
        unsafe { core::ptr::write_volatile((frame_start + offset) as *mut u64, value) };
        Ok(())
    }

    fn copy_into(
        &mut self,
        frame_start: usize,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        let offset = checked_frame_range(frame_start, offset, bytes.len())?;
        // Safety: the destination range was checked before pointer derivation;
        // `ptr::copy` also permits overlap with a source slice in the same frame.
        unsafe {
            core::ptr::copy(
                bytes.as_ptr(),
                (frame_start + offset) as *mut u8,
                bytes.len(),
            )
        };
        Ok(())
    }

    fn copy_out(
        &self,
        frame_start: usize,
        offset: usize,
        output: &mut [u8],
    ) -> Result<(), Self::Error> {
        let offset = checked_frame_range(frame_start, offset, output.len())?;
        // Safety: the source range was checked before pointer derivation;
        // `ptr::copy` also permits overlap with an output slice in the frame.
        unsafe {
            core::ptr::copy(
                (frame_start + offset) as *const u8,
                output.as_mut_ptr(),
                output.len(),
            )
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{IdentityFrameStoreError, checked_frame_range, checked_word_offset};

    // Catches deriving a pointer from an unaligned frame base or accepting the
    // first u64 index beyond a 4 KiB page table frame.
    #[test]
    fn identity_store_rejects_invalid_frame_and_word_boundaries() {
        assert_eq!(
            checked_word_offset(0x1001, 0),
            Err(IdentityFrameStoreError::UnalignedFrame)
        );
        assert_eq!(checked_word_offset(0x1000, 511), Ok(4088));
        assert_eq!(
            checked_word_offset(0x1000, 512),
            Err(IdentityFrameStoreError::IndexOutOfBounds)
        );
    }

    // Catches offset+length overflow and ranges crossing the final byte of a
    // frame before copy_into/copy_out derive a raw pointer.
    #[test]
    fn identity_store_rejects_byte_ranges_outside_one_frame() {
        assert_eq!(checked_frame_range(0x1000, 4095, 1), Ok(4095));
        assert_eq!(checked_frame_range(0x1000, 4096, 0), Ok(4096));
        assert_eq!(
            checked_frame_range(0x1000, 4095, 2),
            Err(IdentityFrameStoreError::RangeOutOfBounds)
        );
        assert_eq!(
            checked_frame_range(0x1000, usize::MAX, 2),
            Err(IdentityFrameStoreError::RangeOutOfBounds)
        );
    }
}
