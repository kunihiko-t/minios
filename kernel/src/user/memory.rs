//! pageごとの検証付きuser memory copy。

use crate::{
    memory::frame::PAGE_SIZE,
    vm::{AddressSpace, FrameStore, VirtAddr, VmError},
};

/// 検証付きcopyが拒絶した理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMemoryError<E> {
    /// 対象pageがuser address spaceに存在しない。
    Unmapped,
    /// 対象pageがU=1かつ読み取り可能ではない。
    Permission,
    /// `start + len`がアドレス空間を超えて表現できない。
    AddressOverflow,
    /// kernel側frame storeの読み出しが失敗した。
    Store(E),
}

/// user仮想rangeの先頭`output.len()`byteを、pageごとの検証を通して
/// kernel bufferへcopyする。
///
/// `start.checked_add(len)`でrangeを先に確定させ、各pageについて`U`と`R`を
/// 確認してから、そのpage内のbyteだけ`FrameStore::copy_out`する。呼び出し側が
/// 用意するbufferは4,096 byte以下でなければならず、user pointerがRustの
/// 参照やraw pointerとして解されることは決してない。
pub fn copy_from_user<const N: usize, M: FrameStore>(
    space: &AddressSpace<'_, N>,
    memory: &M,
    start: u64,
    output: &mut [u8],
) -> Result<(), UserMemoryError<M::Error>> {
    let length = u64::try_from(output.len()).map_err(|_| UserMemoryError::AddressOverflow)?;
    // rangeを先に確定させ、以降のpage walkがこの中に収まるようにする。
    start
        .checked_add(length)
        .ok_or(UserMemoryError::AddressOverflow)?;
    let mut copied = 0usize;
    while copied < output.len() {
        let address = start + copied as u64;
        let page_offset = address as usize % PAGE_SIZE;
        let chunk = core::cmp::min(PAGE_SIZE - page_offset, output.len() - copied);
        let virtual_address = VirtAddr::try_new(address).map_err(|_| UserMemoryError::Unmapped)?;
        let (physical, flags) =
            space
                .translate(memory, virtual_address)
                .map_err(|error| match error {
                    VmError::Store(store) => UserMemoryError::Store(store),
                    _ => UserMemoryError::Unmapped,
                })?;
        if !flags.user() || !flags.read() {
            return Err(UserMemoryError::Permission);
        }
        let frame_start = (physical.as_u64() - page_offset as u64) as usize;
        memory
            .copy_out(
                frame_start,
                page_offset,
                &mut output[copied..copied + chunk],
            )
            .map_err(UserMemoryError::Store)?;
        copied += chunk;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

    use super::{UserMemoryError, copy_from_user};
    use crate::{
        memory::frame::{FrameAllocator, PAGE_SIZE},
        vm::{AddressSpaceBuilder, AddressSpaceStorage, FrameStore, PageFlags, PhysAddr, VirtPage},
    };

    const FIRST_USER_PAGE: u64 = 0x0010_0000;
    // 最初のuser pageの上端であり、同時に次のuser pageの下端でもある。
    const USER_PAGE_END: u64 = 0x0010_1000;
    const SUPERVISOR_PAGE: u64 = 0x8020_0000;
    const UNMAPPED_PAGE: u64 = 0x0030_0000;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestStoreError {
        MissingFrame,
        RangeOutOfBounds,
    }

    #[derive(Default)]
    struct TestFrameStore {
        frames: BTreeMap<usize, Box<[u8; PAGE_SIZE]>>,
    }

    impl TestFrameStore {
        fn frame(&self, frame_start: usize) -> Result<&[u8; PAGE_SIZE], TestStoreError> {
            self.frames
                .get(&frame_start)
                .map(Box::as_ref)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn frame_mut(
            &mut self,
            frame_start: usize,
        ) -> Result<&mut [u8; PAGE_SIZE], TestStoreError> {
            self.frames
                .get_mut(&frame_start)
                .map(Box::as_mut)
                .ok_or(TestStoreError::MissingFrame)
        }

        fn range(offset: usize, len: usize) -> Result<core::ops::Range<usize>, TestStoreError> {
            let end = offset
                .checked_add(len)
                .ok_or(TestStoreError::RangeOutOfBounds)?;
            if end > PAGE_SIZE {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            Ok(offset..end)
        }
    }

    impl FrameStore for TestFrameStore {
        type Error = TestStoreError;

        fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error> {
            self.frames.insert(frame_start, Box::new([0; PAGE_SIZE]));
            Ok(())
        }

        fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            let offset = index * 8;
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&self.frame(frame_start)?[offset..offset + 8]);
            Ok(u64::from_le_bytes(bytes))
        }

        fn write_u64(
            &mut self,
            frame_start: usize,
            index: usize,
            value: u64,
        ) -> Result<(), Self::Error> {
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::RangeOutOfBounds);
            }
            let offset = index * 8;
            self.frame_mut(frame_start)?[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            Ok(())
        }

        fn copy_into(
            &mut self,
            frame_start: usize,
            offset: usize,
            bytes: &[u8],
        ) -> Result<(), Self::Error> {
            let range = Self::range(offset, bytes.len())?;
            self.frame_mut(frame_start)?[range].copy_from_slice(bytes);
            Ok(())
        }

        fn copy_out(
            &self,
            frame_start: usize,
            offset: usize,
            output: &mut [u8],
        ) -> Result<(), Self::Error> {
            let range = Self::range(offset, output.len())?;
            output.copy_from_slice(&self.frame(frame_start)?[range]);
            Ok(())
        }
    }

    // 2枚の読めるuser page ('a','b'を1枚目の末尾、'c','d'を2枚目の先頭へ)、
    // U=0のborrowed supervisor pageを持ち、それ以外は未mapの空間を返す。
    fn copy_fixture(start: u64, len: usize) -> Result<Vec<u8>, UserMemoryError<TestStoreError>> {
        let mut allocator = unsafe { FrameAllocator::<16>::new(0x1000, 0x41_000) }.unwrap();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();
        let mut builder =
            AddressSpaceBuilder::new(&mut allocator, &mut memory, &mut storage).unwrap();
        let user_flags = PageFlags::new(true, true, false, true).unwrap();
        let first = builder
            .map_new_zeroed(VirtPage::from_start(FIRST_USER_PAGE).unwrap(), user_flags)
            .unwrap();
        builder
            .copy_into(first, (USER_PAGE_END - FIRST_USER_PAGE) as usize - 2, b"ab")
            .unwrap();
        let second = builder
            .map_new_zeroed(VirtPage::from_start(USER_PAGE_END).unwrap(), user_flags)
            .unwrap();
        builder.copy_into(second, 0, b"cd").unwrap();
        builder
            .map_borrowed(
                VirtPage::from_start(SUPERVISOR_PAGE).unwrap(),
                PhysAddr::try_new(SUPERVISOR_PAGE).unwrap(),
                PageFlags::supervisor_r(),
            )
            .unwrap();
        let space = builder.finish();

        let mut output = vec![0u8; len];
        copy_from_user(&space, &memory, start, &mut output).map(|()| output)
    }

    // Catches granting copy access to supervisor pages or unmapped pages.
    #[test]
    fn copy_rejects_supervisor_and_unmapped_pages() {
        assert_eq!(
            copy_fixture(SUPERVISOR_PAGE, 1),
            Err(UserMemoryError::Permission)
        );
        assert_eq!(
            copy_fixture(UNMAPPED_PAGE, 1),
            Err(UserMemoryError::Unmapped)
        );
    }

    // Catches overreading past the requested length, copying only part of a
    // page-crossing range, or failing the second page walk.
    #[test]
    fn copy_crosses_two_readable_user_pages_without_overread() {
        assert_eq!(
            copy_fixture(USER_PAGE_END - 2, 4),
            Ok(vec![b'a', b'b', b'c', b'd'])
        );
    }

    // Catches treating an execute-only user page as readable.
    #[test]
    fn copy_rejects_user_pages_without_read_permission() {
        let mut allocator = unsafe { FrameAllocator::<8>::new(0x1000, 0x21_000) }.unwrap();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<8>::new();
        let mut builder =
            AddressSpaceBuilder::new(&mut allocator, &mut memory, &mut storage).unwrap();
        builder
            .map_new_zeroed(
                VirtPage::from_start(FIRST_USER_PAGE).unwrap(),
                PageFlags::new(false, false, true, true).unwrap(),
            )
            .unwrap();
        let space = builder.finish();

        let mut output = [0u8; 1];
        assert_eq!(
            copy_from_user(&space, &memory, FIRST_USER_PAGE, &mut output),
            Err(UserMemoryError::Permission)
        );
    }

    // Catches wrapping the end address instead of rejecting the range.
    #[test]
    fn copy_rejects_ranges_that_overflow_the_address_space() {
        assert_eq!(
            copy_fixture(u64::MAX - 1, 4),
            Err(UserMemoryError::AddressOverflow)
        );
    }
}
