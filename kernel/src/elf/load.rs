use core::{cmp, fmt};

use crate::{
    elf::{ElfError, ElfImage, LoadPlan, LoadSegment, USER_STACK_BOTTOM, USER_STACK_TOP},
    memory::frame::{FrameAllocator, FrameError, PAGE_SIZE},
    vm::{
        AddressSpace, AddressSpaceBuilder, AddressSpaceStorage, FrameKind, FrameStore,
        KernelMapping, PageFlags, VirtAddr, VirtPage, VmError,
    },
};

#[derive(Debug, PartialEq, Eq)]
/// A typed failure from validation, VM construction, or direct byte copying.
pub enum LoadError<E> {
    /// ELF parsing or complete load-plan validation failed before allocation.
    Elf(ElfError),
    /// A borrowed kernel mapping exposed a page to U-mode.
    UserAccessibleKernelMapping,
    /// Address-space construction failed.
    Vm(VmError<E>),
    /// Copying validated segment bytes into a mapped frame failed.
    Memory(E),
}

/// An inactive user image together with its owned address space and metadata.
pub struct LoadedImage<'storage, const N: usize> {
    address_space: AddressSpace<'storage, N>,
    entry: VirtAddr,
    user_stack_top: VirtAddr,
}

/// A failed destruction that retains the complete image for a safe retry.
pub struct LoadedImageDestroyError<'storage, const N: usize> {
    frame_error: FrameError,
    image: LoadedImage<'storage, N>,
}

impl<'storage, const N: usize> LoadedImageDestroyError<'storage, N> {
    /// Returns the allocator rejection without consuming retryable ownership.
    pub const fn frame_error(&self) -> FrameError {
        self.frame_error
    }

    /// Returns both the rejection and the complete image for another attempt.
    pub fn into_parts(self) -> (FrameError, LoadedImage<'storage, N>) {
        (self.frame_error, self.image)
    }
}

impl<const N: usize> fmt::Debug for LoadedImageDestroyError<'_, N> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedImageDestroyError")
            .field("frame_error", &self.frame_error)
            .finish_non_exhaustive()
    }
}

impl<'storage, const N: usize> LoadedImage<'storage, N> {
    /// Returns the validated executable entry address.
    pub const fn entry(&self) -> VirtAddr {
        self.entry
    }

    /// Returns the exclusive upper bound of the zeroed user stack.
    pub const fn user_stack_top(&self) -> VirtAddr {
        self.user_stack_top
    }

    /// Borrows the inactive address space for translation and inspection.
    pub const fn address_space(&self) -> &AddressSpace<'storage, N> {
        &self.address_space
    }

    pub(crate) const fn allocator_id(&self) -> u64 {
        self.address_space.allocator_id()
    }

    /// Returns every owned frame, retaining this image if the allocator rejects it.
    pub fn destroy<const WORDS: usize>(
        self,
        allocator: &mut FrameAllocator<WORDS>,
    ) -> Result<(), LoadedImageDestroyError<'storage, N>> {
        let Self {
            address_space,
            entry,
            user_stack_top,
        } = self;
        match address_space.destroy(allocator) {
            Ok(()) => Ok(()),
            Err(error) => {
                let (frame_error, address_space) = error.into_parts();
                Err(LoadedImageDestroyError {
                    frame_error,
                    image: Self {
                        address_space,
                        entry,
                        user_stack_top,
                    },
                })
            }
        }
    }
}

/// Validates an ELF completely, then materializes it into an inactive address space.
///
/// Host unit tests use this entry point with an empty kernel-mapping iterator;
/// the U-mode runtime borrows the kernel identity pages through
/// [`load_image_with_kernel_mappings`] instead.
pub fn load_image<'storage, const N: usize, const WORDS: usize, M: FrameStore>(
    bytes: &[u8],
    allocator: &mut FrameAllocator<WORDS>,
    memory: &mut M,
    storage: &'storage mut AddressSpaceStorage<N>,
) -> Result<LoadedImage<'storage, N>, LoadError<M::Error>> {
    load_image_with_kernel_mappings(bytes, allocator, memory, storage, core::iter::empty())
}

/// Validates an ELF completely, then materializes it into an inactive address
/// space that also borrows every kernel mapping as supervisor-only pages.
///
/// The kernel mappings must be installed before the user pages so that the
/// U-mode entry, trap stack, and console stay reachable the moment `sret`
/// switches `satp`. A mapping with `U=1` is rejected before it can become a
/// borrowed leaf, so only user-image pages can be user-accessible.
pub fn load_image_with_kernel_mappings<
    'storage,
    const N: usize,
    const WORDS: usize,
    M: FrameStore,
    I: IntoIterator<Item = KernelMapping>,
>(
    bytes: &[u8],
    allocator: &mut FrameAllocator<WORDS>,
    memory: &mut M,
    storage: &'storage mut AddressSpaceStorage<N>,
    kernel_mappings: I,
) -> Result<LoadedImage<'storage, N>, LoadError<M::Error>> {
    let image = ElfImage::parse(bytes).map_err(LoadError::Elf)?;
    let plan = LoadPlan::new(&image).map_err(LoadError::Elf)?;
    let entry = plan.entry();
    let user_stack_top =
        VirtAddr::try_new(USER_STACK_TOP).map_err(|_| LoadError::Elf(ElfError::RangeOverflow))?;

    let mut builder =
        AddressSpaceBuilder::new(allocator, memory, storage).map_err(LoadError::Vm)?;
    for mapping in kernel_mappings {
        if mapping.flags().user() {
            return Err(LoadError::UserAccessibleKernelMapping);
        }
        builder
            .map_borrowed(mapping.page(), mapping.physical(), mapping.flags())
            .map_err(LoadError::Vm)?;
    }
    for segment in plan.segments() {
        materialize_segment(bytes, &mut builder, segment)?;
    }

    let stack_flags = PageFlags::new(true, true, false, true)
        .map_err(|error| LoadError::Vm(VmError::Pte(error)))?;
    for stack_start in (USER_STACK_BOTTOM..USER_STACK_TOP).step_by(PAGE_SIZE) {
        let page = VirtPage::from_start(stack_start)
            .map_err(|error| LoadError::Vm(VmError::Address(error)))?;
        builder
            .map_new_zeroed_with_kind(page, stack_flags, FrameKind::Stack)
            .map_err(LoadError::Vm)?;
    }

    Ok(LoadedImage {
        address_space: builder.finish(),
        entry,
        user_stack_top,
    })
}

fn materialize_segment<const N: usize, const WORDS: usize, M: FrameStore>(
    bytes: &[u8],
    builder: &mut AddressSpaceBuilder<'_, '_, '_, N, WORDS, M>,
    segment: &LoadSegment,
) -> Result<(), LoadError<M::Error>> {
    let file_len =
        u64::try_from(segment.file_len()).map_err(|_| LoadError::Elf(ElfError::RangeOverflow))?;
    let file_virtual_end = segment
        .virtual_start()
        .as_u64()
        .checked_add(file_len)
        .ok_or(LoadError::Elf(ElfError::RangeOverflow))?;

    for page_index in 0..segment.page_count() {
        let page_delta = page_index
            .checked_mul(PAGE_SIZE)
            .ok_or(LoadError::Elf(ElfError::RangeOverflow))?;
        let page_delta =
            u64::try_from(page_delta).map_err(|_| LoadError::Elf(ElfError::RangeOverflow))?;
        let page_start = segment
            .first_page()
            .start()
            .as_u64()
            .checked_add(page_delta)
            .ok_or(LoadError::Elf(ElfError::RangeOverflow))?;
        let page_end = page_start
            .checked_add(PAGE_SIZE as u64)
            .ok_or(LoadError::Elf(ElfError::RangeOverflow))?;
        let page = VirtPage::from_start(page_start)
            .map_err(|error| LoadError::Vm(VmError::Address(error)))?;
        let mapped = builder
            .map_new_zeroed_with_kind(page, segment.flags(), FrameKind::User)
            .map_err(LoadError::Vm)?;

        let copy_start = cmp::max(page_start, segment.virtual_start().as_u64());
        let copy_end = cmp::min(page_end, file_virtual_end);
        if copy_start >= copy_end {
            continue;
        }

        let source_delta = usize::try_from(copy_start - segment.virtual_start().as_u64())
            .map_err(|_| LoadError::Elf(ElfError::RangeOverflow))?;
        let source_start = segment
            .file_offset()
            .checked_add(source_delta)
            .ok_or(LoadError::Elf(ElfError::RangeOverflow))?;
        let copy_len = usize::try_from(copy_end - copy_start)
            .map_err(|_| LoadError::Elf(ElfError::RangeOverflow))?;
        let source_end = source_start
            .checked_add(copy_len)
            .ok_or(LoadError::Elf(ElfError::RangeOverflow))?;
        let source = bytes
            .get(source_start..source_end)
            .ok_or(LoadError::Elf(ElfError::RangeOverflow))?;
        let destination_offset = usize::try_from(copy_start - page_start)
            .map_err(|_| LoadError::Elf(ElfError::RangeOverflow))?;
        builder
            .copy_into(mapped, destination_offset, source)
            .map_err(LoadError::Memory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{boxed::Box, collections::BTreeMap, vec, vec::Vec};

    use super::{LoadError, LoadedImage, load_image, load_image_with_kernel_mappings};
    use crate::{
        elf::{USER_GUARD_BOTTOM, USER_STACK_BOTTOM, USER_STACK_TOP, fixture},
        memory::{
            KernelSections,
            frame::{FrameAllocator, FrameError, PAGE_SIZE},
        },
        vm::{
            AddressSpaceStorage, FrameStore, KernelMapPlan, KernelMapping, PageFlags, PhysAddr,
            VirtAddr, VirtPage, VmError,
        },
    };

    const FIRST_HEADER: usize = 64;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestStoreError {
        MissingFrame,
        IndexOutOfBounds,
        RangeOutOfBounds,
        InjectedZeroFailure,
        InjectedWriteFailure,
        InjectedCopyFailure,
    }

    #[derive(Default)]
    struct TestFrameStore {
        frames: BTreeMap<usize, Box<[u8; PAGE_SIZE]>>,
        zeroes: usize,
        writes: usize,
        copies: usize,
        fail_zero_after: Option<usize>,
        fail_write_after: Option<usize>,
        fail_copy_after: Option<usize>,
    }

    impl TestFrameStore {
        fn fail_zero_after(successes: usize) -> Self {
            Self {
                fail_zero_after: Some(successes),
                ..Self::default()
            }
        }

        fn fail_write_after(successes: usize) -> Self {
            Self {
                fail_write_after: Some(successes),
                ..Self::default()
            }
        }

        fn fail_copy_after(successes: usize) -> Self {
            Self {
                fail_copy_after: Some(successes),
                ..Self::default()
            }
        }

        fn clear_failures(&mut self) {
            self.fail_zero_after = None;
            self.fail_write_after = None;
            self.fail_copy_after = None;
        }

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
            if self.fail_zero_after == Some(self.zeroes) {
                return Err(TestStoreError::InjectedZeroFailure);
            }
            self.zeroes += 1;
            self.frames.insert(frame_start, Box::new([0; PAGE_SIZE]));
            Ok(())
        }

        fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error> {
            if index >= PAGE_SIZE / 8 {
                return Err(TestStoreError::IndexOutOfBounds);
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
                return Err(TestStoreError::IndexOutOfBounds);
            }
            if self.fail_write_after == Some(self.writes) {
                return Err(TestStoreError::InjectedWriteFailure);
            }
            self.writes += 1;
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
            if self.fail_copy_after == Some(self.copies) {
                return Err(TestStoreError::InjectedCopyFailure);
            }
            self.copies += 1;
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

    fn test_allocator<const WORDS: usize>(base: usize, end: usize) -> FrameAllocator<WORDS> {
        // Safety: the host store models these addresses and never dereferences them.
        unsafe { FrameAllocator::new(base, end) }.unwrap()
    }

    fn fixture_allocator() -> FrameAllocator<1> {
        test_allocator(0x1000, 0x41_000)
    }

    fn kernel_plan_fixture() -> KernelMapPlan {
        let sections = KernelSections::new(
            0x8020_0000..0x8020_2000,
            0x8020_2000..0x8020_3000,
            0x8020_3000..0x8020_5000,
            0x8020_5000..0x8021_5000,
            0x8021_5000,
        )
        .unwrap();
        KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000).unwrap()
    }

    // Catches borrowing kernel pages with the U bit set, dropping a borrowed
    // range, or letting the U bit leak onto any user page. Mirrors the exact
    // production borrow list the U-mode entry probe installs in QEMU.
    #[test]
    fn user_space_borrows_kernel_pages_supervisor_only_and_maps_only_user_pages_as_user() {
        let bytes = fixture::valid_riscv64_elf();
        let plan = kernel_plan_fixture();
        let mut allocator = test_allocator::<16>(0x1000, 0x181_000);
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();

        let image = load_image_with_kernel_mappings(
            &bytes,
            &mut allocator,
            &mut memory,
            &mut storage,
            plan.mappings(),
        )
        .unwrap();

        for (region, address) in [
            ("kernel text", 0x8020_0000u64),
            ("boot stack", 0x8020_5000),
            ("managed RAM", 0x8021_5000),
            ("UART", 0x1000_0000),
        ] {
            let (_, flags) = image
                .address_space()
                .translate(&memory, VirtAddr::try_new(address).unwrap())
                .unwrap_or_else(|error| panic!("{region} must stay reachable: {error:?}"));
            assert!(!flags.user(), "{region} @ {address:#x} must keep U=0");
        }

        let user_text = image
            .address_space()
            .translate(&memory, VirtAddr::try_new(0x0010_0000).unwrap())
            .unwrap()
            .1;
        assert_eq!(user_text, PageFlags::new(true, false, true, true).unwrap());
        let user_stack = image
            .address_space()
            .translate(&memory, VirtAddr::try_new(USER_STACK_BOTTOM).unwrap())
            .unwrap()
            .1;
        assert_eq!(user_stack, PageFlags::new(true, true, false, true).unwrap());

        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats().allocated, 0);
        assert_eq!(storage.len(), 0);
    }

    // Catches accepting a caller-supplied borrowed kernel mapping with U=1,
    // which would make a kernel identity page readable from U-mode after sret.
    #[test]
    fn load_rejects_user_accessible_borrowed_kernel_mappings() {
        let bytes = fixture::valid_riscv64_elf();
        let mapping = KernelMapping::new(
            VirtPage::from_start(0x8020_0000).unwrap(),
            PhysAddr::try_new(0x8020_0000).unwrap(),
            PageFlags::new(true, false, true, true).unwrap(),
        );
        let mut allocator = test_allocator::<16>(0x1000, 0x181_000);
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();

        assert!(matches!(
            load_image_with_kernel_mappings(
                &bytes,
                &mut allocator,
                &mut memory,
                &mut storage,
                [mapping],
            ),
            Err(LoadError::UserAccessibleKernelMapping)
        ));
    }

    fn read_virtual<const N: usize>(
        image: &LoadedImage<'_, N>,
        memory: &TestFrameStore,
        start: u64,
        len: usize,
    ) -> Vec<u8> {
        let mut output = vec![0; len];
        let mut completed = 0usize;
        while completed < len {
            let address = start + completed as u64;
            let (physical, _) = image
                .address_space()
                .translate(memory, VirtAddr::try_new(address).unwrap())
                .unwrap();
            let offset = physical.page_offset();
            let count = (PAGE_SIZE - offset).min(len - completed);
            let frame_start = physical.as_u64() as usize - offset;
            memory
                .copy_out(
                    frame_start,
                    offset,
                    &mut output[completed..completed + count],
                )
                .unwrap();
            completed += count;
        }
        output
    }

    fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    // Catches copying only the first page, losing BSS/partial-page zeroes,
    // mapping the guard, or assigning broader/narrower segment/stack flags.
    #[test]
    fn materializes_fixture_bytes_bss_stack_guard_and_permissions() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();

        let image = load_image(&bytes, &mut allocator, &mut memory, &mut storage).unwrap();

        assert_eq!(image.entry().as_u64(), 0x0010_0000);
        assert_eq!(image.user_stack_top().as_u64(), USER_STACK_TOP);
        assert_eq!(
            read_virtual(&image, &memory, 0x0010_0000, 4),
            [0x13, 0, 0, 0]
        );
        assert!(
            read_virtual(&image, &memory, 0x0010_0004, PAGE_SIZE - 4)
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(read_virtual(&image, &memory, 0x0020_0000, 4), b"MCB1");
        assert!(
            read_virtual(&image, &memory, 0x0020_0004, PAGE_SIZE - 4)
                .iter()
                .all(|byte| *byte == 0)
        );

        for stack_page in (USER_STACK_BOTTOM..USER_STACK_TOP).step_by(PAGE_SIZE) {
            assert!(
                read_virtual(&image, &memory, stack_page, PAGE_SIZE)
                    .iter()
                    .all(|byte| *byte == 0)
            );
        }
        assert_eq!(
            image
                .address_space()
                .translate(&memory, VirtAddr::try_new(USER_GUARD_BOTTOM).unwrap(),),
            Err(VmError::NotMapped)
        );

        let text_flags = image
            .address_space()
            .translate(&memory, VirtAddr::try_new(0x0010_0000).unwrap())
            .unwrap()
            .1;
        assert_eq!(text_flags, PageFlags::new(true, false, true, true).unwrap());
        let data_flags = image
            .address_space()
            .translate(&memory, VirtAddr::try_new(0x0020_0000).unwrap())
            .unwrap()
            .1;
        assert_eq!(data_flags, PageFlags::new(true, true, false, true).unwrap());
        let stack_flags = image
            .address_space()
            .translate(&memory, VirtAddr::try_new(USER_STACK_BOTTOM).unwrap())
            .unwrap()
            .1;
        assert_eq!(
            stack_flags,
            PageFlags::new(true, true, false, true).unwrap()
        );
    }

    // Catches fixture/page-table growth invalidating the QEMU harness's
    // 64-frame dirty-reuse budget. The current image owns five page-table
    // frames, two segment frames, and sixteen stack frames.
    #[test]
    fn fixture_owns_exactly_twenty_three_frames() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();

        let image = load_image(&bytes, &mut allocator, &mut memory, &mut storage).unwrap();

        assert_eq!(allocator.stats().allocated - before.allocated, 23);
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    // Catches page-copy arithmetic that assumes aligned segments or uses the
    // same source/destination offsets when file bytes cross a page boundary.
    #[test]
    fn copies_unaligned_segment_intersections_across_page_boundaries() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_u64(&mut bytes, 24, 0x0010_0ff0);
        set_u64(&mut bytes, FIRST_HEADER + 8, 0x0ff0);
        set_u64(&mut bytes, FIRST_HEADER + 16, 0x0010_0ff0);
        set_u64(&mut bytes, FIRST_HEADER + 32, 32);
        set_u64(&mut bytes, FIRST_HEADER + 40, 48);
        for (index, byte) in bytes[0x0ff0..0x1010].iter_mut().enumerate() {
            *byte = index as u8;
        }
        let mut allocator = fixture_allocator();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();

        let image = load_image(&bytes, &mut allocator, &mut memory, &mut storage).unwrap();

        assert_eq!(
            read_virtual(&image, &memory, 0x0010_0ff0, 32),
            (0_u8..32).collect::<Vec<_>>()
        );
        assert_eq!(read_virtual(&image, &memory, 0x0010_1010, 16), [0; 16]);
        assert!(
            read_virtual(&image, &memory, 0x0010_0000, 0x0ff0)
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    // Catches constructing a builder before header parsing and thereby
    // allocating/zeroing a root frame for an invalid ELF envelope.
    #[test]
    fn invalid_elf_is_rejected_before_any_allocation() {
        let bytes = [0_u8; 16];
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();

        assert!(matches!(
            load_image(&bytes, &mut allocator, &mut memory, &mut storage),
            Err(LoadError::Elf(_))
        ));
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
        assert_eq!(memory.zeroes, 0);
    }

    // Catches beginning allocation after header parsing but before the full
    // load plan has rejected a forbidden W+X segment.
    #[test]
    fn invalid_load_plan_is_rejected_before_any_allocation() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_u32(&mut bytes, FIRST_HEADER + 4, 7);
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();

        assert!(matches!(
            load_image(&bytes, &mut allocator, &mut memory, &mut storage),
            Err(LoadError::Elf(_))
        ));
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
        assert_eq!(memory.zeroes, 0);
    }

    // Catches translating a direct file-copy failure into Vm or leaving any
    // owned frame/storage entry behind after the second copy operation fails.
    #[test]
    fn copy_failure_is_memory_error_and_rolls_back_for_storage_reuse() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::fail_copy_after(1);
        let mut storage = AddressSpaceStorage::<2688>::new();

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory, &mut storage)
                .err()
                .unwrap(),
            LoadError::Memory(TestStoreError::InjectedCopyFailure)
        );
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);

        memory.clear_failures();
        let image = load_image(&bytes, &mut allocator, &mut memory, &mut storage).unwrap();
        assert_eq!(read_virtual(&image, &memory, 0x0020_0000, 4), b"MCB1");
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    // Catches dropping the builder without recovering frames after a zeroing
    // failure that occurs after earlier page-table allocations succeeded.
    #[test]
    fn vm_zero_failure_rolls_back_every_owned_frame() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::fail_zero_after(2);
        let mut storage = AddressSpaceStorage::<2688>::new();

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory, &mut storage)
                .err()
                .unwrap(),
            LoadError::Vm(VmError::Store(TestStoreError::InjectedZeroFailure))
        );
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    // Catches treating page-table writes as direct memory-copy failures or
    // retaining frames after a VM installation failure.
    #[test]
    fn vm_write_failure_rolls_back_every_owned_frame() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::fail_write_after(1);
        let mut storage = AddressSpaceStorage::<2688>::new();

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory, &mut storage)
                .err()
                .unwrap(),
            LoadError::Vm(VmError::Store(TestStoreError::InjectedWriteFailure))
        );
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    // Catches losing already allocated frames when fixed ownership storage is
    // too small to record the next page-table/user frame.
    #[test]
    fn ownership_capacity_failure_rolls_back_every_frame() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2>::new();

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory, &mut storage)
                .err()
                .unwrap(),
            LoadError::Vm(VmError::CapacityExceeded)
        );
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    // Catches leaking the root or intermediate frame when physical frames run
    // out before the first user leaf can be installed.
    #[test]
    fn allocator_exhaustion_rolls_back_every_frame() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = test_allocator::<1>(0x1000, 0x3000);
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory, &mut storage)
                .err()
                .unwrap(),
            LoadError::Vm(VmError::OutOfFrames)
        );
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    // Catches normal destruction retaining any frame/storage token and proves
    // a reused executable frame is overwritten by the next complete image.
    #[test]
    fn successful_destroy_restores_allocator_and_reuses_storage() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();
        let entry = VirtAddr::try_new(0x0010_0000).unwrap();

        let image = load_image(&bytes, &mut allocator, &mut memory, &mut storage).unwrap();
        let first_text_frame = image.address_space().translate(&memory, entry).unwrap().0;
        assert_eq!(
            read_virtual(&image, &memory, 0x0010_0000, 4),
            [0x13, 0, 0, 0]
        );
        assert!(allocator.stats().allocated > 0);
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);

        let mut replacement = fixture::valid_riscv64_elf();
        replacement[0x1000..0x1004].copy_from_slice(&0x0010_0073_u32.to_le_bytes());
        let second = load_image(&replacement, &mut allocator, &mut memory, &mut storage).unwrap();
        let second_text_frame = second.address_space().translate(&memory, entry).unwrap().0;
        assert_eq!(second_text_frame, first_text_frame);
        assert_eq!(
            read_virtual(&second, &memory, 0x0010_0000, 4),
            0x0010_0073_u32.to_le_bytes()
        );
        second.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    // Catches consuming LoadedImage metadata/ownership on a different-range
    // allocator error instead of returning a complete retryable image.
    #[test]
    fn wrong_allocator_destroy_preserves_image_for_origin_retry() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut other = test_allocator::<1>(0x80_000, 0x81_000);
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();
        let image = load_image(&bytes, &mut allocator, &mut memory, &mut storage).unwrap();
        let loaded = allocator.stats();

        let failure = image.destroy(&mut other).unwrap_err();
        assert_eq!(failure.frame_error(), FrameError::WrongAllocator);
        assert_eq!(allocator.stats(), loaded);
        assert_eq!(other.stats().allocated, 0);

        let (error, image) = failure.into_parts();
        assert_eq!(error, FrameError::WrongAllocator);
        assert_eq!(image.entry().as_u64(), 0x0010_0000);
        assert_eq!(image.user_stack_top().as_u64(), USER_STACK_TOP);
        assert_eq!(read_virtual(&image, &memory, 0x0020_0000, 4), b"MCB1");
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }

    // Catches accepting an allocator merely because its numeric range matches;
    // provenance must preserve the image for retry with the originating instance.
    #[test]
    fn same_range_distinct_allocator_destroy_is_retryable() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        // Safety: this test intentionally constructs a conflicting bitmap model
        // without dereferencing its synthetic addresses to exercise provenance.
        let mut same_range = unsafe { FrameAllocator::<1>::new(0x1000, 0x41_000) }.unwrap();
        let mut memory = TestFrameStore::default();
        let mut storage = AddressSpaceStorage::<2688>::new();
        let image = load_image(&bytes, &mut allocator, &mut memory, &mut storage).unwrap();
        let loaded = allocator.stats();

        let failure = image.destroy(&mut same_range).unwrap_err();
        assert_eq!(failure.frame_error(), FrameError::WrongAllocator);
        assert_eq!(allocator.stats(), loaded);
        assert_eq!(same_range.stats().allocated, 0);
        let (_, image) = failure.into_parts();
        assert_eq!(image.entry().as_u64(), 0x0010_0000);
        assert_eq!(
            image
                .address_space()
                .translate(&memory, VirtAddr::try_new(USER_STACK_BOTTOM).unwrap())
                .unwrap()
                .1,
            PageFlags::new(true, true, false, true).unwrap()
        );
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
        assert_eq!(storage.len(), 0);
    }
}
