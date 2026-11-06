use alloc::{vec, vec::Vec};
use core::{cmp, fmt};

use crate::{
    elf::{
        ElfError, ElfImage, LoadPlan, LoadSegment, MAX_USER_IMAGE_PAGES, USER_GUARD_BOTTOM,
        USER_STACK_BOTTOM, USER_STACK_TOP,
        header::{ELF_HEADER_LEN, ElfHeader},
    },
    memory::frame::{FrameError, FrameSource, PAGE_SIZE},
    vm::{
        AddressSpace, AddressSpaceBuilder, FrameKind, FrameStore, KernelMapping, PageFlags,
        VirtAddr, VirtPage, VmError,
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
    /// The [`ElfSource`] could not supply bytes it reported as present.
    ///
    /// The source keeps its own error; the caller asks it for the cause.
    SourceRead,
}

/// A random-access view of an ELF file that the loader reads piecewise.
///
/// The loader reads the file header, then the program-header table, then
/// each loadable segment one page at a time, so only bytes that end up in
/// the image (plus a bounded table) are ever buffered. Section data such as
/// debug information is never read.
pub trait ElfSource {
    /// Returns the whole file length in bytes.
    fn len(&self) -> usize;

    /// Returns whether the file is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fills `output` with the bytes starting at `offset`.
    ///
    /// The loader only asks for ranges inside `len()`. Returning `Err(())`
    /// aborts the load with [`LoadError::SourceRead`].
    #[allow(clippy::result_unit_err)]
    fn read_exact_at(&mut self, offset: usize, output: &mut [u8]) -> Result<(), ()>;
}

impl<T: AsRef<[u8]> + ?Sized> ElfSource for &T {
    fn len(&self) -> usize {
        (*self).as_ref().len()
    }

    fn read_exact_at(&mut self, offset: usize, output: &mut [u8]) -> Result<(), ()> {
        let end = offset.checked_add(output.len()).ok_or(())?;
        output.copy_from_slice((*self).as_ref().get(offset..end).ok_or(())?);
        Ok(())
    }
}

impl<S: ElfSource + ?Sized> ElfSource for &mut S {
    fn len(&self) -> usize {
        (**self).len()
    }

    fn read_exact_at(&mut self, offset: usize, output: &mut [u8]) -> Result<(), ()> {
        (**self).read_exact_at(offset, output)
    }
}

/// An inactive user image together with its owned address space and metadata.
pub struct LoadedImage {
    address_space: AddressSpace,
    entry: VirtAddr,
    user_stack_top: VirtAddr,
    heap: UserHeap,
}

/// `sbrk`のheap状態。heapはimage末尾（最上位PT_LOADのpage境界）から
/// `USER_GUARD_BOTTOM`へ向けて上へ伸びる。`brk`はguestへ見せるbyte単位の
/// break、`mapped_end`は既にmap済みのpage境界であり、frame確保が途中で
/// 失敗した場合は`brk <= mapped_end`のまま後者だけが進む。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UserHeap {
    image_pages: usize,
    start: u64,
    brk: u64,
    mapped_end: u64,
}

/// A failed destruction that retains the complete image for a safe retry.
pub struct LoadedImageDestroyError {
    frame_error: FrameError,
    image: LoadedImage,
}

impl LoadedImageDestroyError {
    /// Returns the allocator rejection without consuming retryable ownership.
    pub const fn frame_error(&self) -> FrameError {
        self.frame_error
    }

    /// Returns both the rejection and the complete image for another attempt.
    pub fn into_parts(self) -> (FrameError, LoadedImage) {
        (self.frame_error, self.image)
    }
}

impl fmt::Debug for LoadedImageDestroyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoadedImageDestroyError")
            .field("frame_error", &self.frame_error)
            .finish_non_exhaustive()
    }
}

impl LoadedImage {
    /// Returns the validated executable entry address.
    pub const fn entry(&self) -> VirtAddr {
        self.entry
    }

    /// Returns the exclusive upper bound of the zeroed user stack.
    pub const fn user_stack_top(&self) -> VirtAddr {
        self.user_stack_top
    }

    /// Borrows the inactive address space for translation and inspection.
    pub const fn address_space(&self) -> &AddressSpace {
        &self.address_space
    }

    /// kernel trap stackのguard pageを外すためにinactiveなspaceを借りる。
    pub(crate) const fn address_space_mut(&mut self) -> &mut AddressSpace {
        &mut self.address_space
    }

    pub(crate) const fn allocator_id(&self) -> u64 {
        self.address_space.allocator_id()
    }

    /// Returns the current `sbrk` break.
    pub const fn brk(&self) -> u64 {
        self.heap.brk
    }

    /// `sbrk`: breakを`increment` byte動かし、旧breakを返す。0は現在値を
    /// 返すだけである。
    ///
    /// 正のincrementは新しいpageをzero済みのU+R+W（Xなし）でmapする。
    /// image page数とheap page数の合計が`MAX_USER_IMAGE_PAGES`を超える、
    /// またはbreakが`USER_GUARD_BOTTOM`を越える要求は何も変えず`ENOMEM`。
    /// frame確保が途中で尽きた場合はmap済みpageを残し（address spaceが
    /// 所有しexit時に回収する）、breakは動かさず`ENOMEM`を返す。
    ///
    /// 負のincrementはbreakを戻し、新しいbreakより上で丸ごと空いたpageを
    /// unmapしてframeをallocatorへ返す。heapの先頭より下へ戻す要求は何も
    /// 変えず`EINVAL`。一度返したpageを再び伸ばすと、zero済みの新しい
    /// frameになる。activeなspaceでは呼び出し側が`sfence.vma`する。
    pub fn sbrk<M: FrameStore>(
        &mut self,
        increment: isize,
        allocator: &mut dyn FrameSource,
        memory: &mut M,
    ) -> Result<u64, isize> {
        use minios_abi::syscall::{EINVAL, ENOMEM};

        let old = self.heap.brk;
        let page = PAGE_SIZE as u64;
        if increment < 0 {
            let new_brk = old
                .checked_sub(increment.unsigned_abs() as u64)
                .filter(|brk| *brk >= self.heap.start)
                .ok_or(EINVAL)?;
            let new_end = new_brk.next_multiple_of(page);
            // 上から順に外すため、途中で返却に失敗してもmap済み範囲は
            // `[start, mapped_end)`の連続のまま保たれる。
            while self.heap.mapped_end > new_end {
                let last = VirtPage::from_start(self.heap.mapped_end - page).map_err(|_| ENOMEM)?;
                self.address_space
                    .unmap_owned_user(allocator, memory, last)
                    .map_err(|_| ENOMEM)?;
                self.heap.mapped_end -= page;
            }
            self.heap.brk = new_brk;
            return Ok(old);
        }

        let increment = increment.unsigned_abs() as u64;
        let new_brk = old
            .checked_add(increment)
            .filter(|end| *end <= USER_GUARD_BOTTOM)
            .ok_or(ENOMEM)?;
        let new_end = new_brk.next_multiple_of(page);
        let heap_pages = usize::try_from((new_end - self.heap.start) / page).map_err(|_| ENOMEM)?;
        if self.heap.image_pages + heap_pages > MAX_USER_IMAGE_PAGES {
            return Err(ENOMEM);
        }

        let flags = PageFlags::new(true, true, false, true).map_err(|_| ENOMEM)?;
        while self.heap.mapped_end < new_end {
            let next = VirtPage::from_start(self.heap.mapped_end).map_err(|_| ENOMEM)?;
            self.address_space
                .map_new_zeroed(allocator, memory, next, flags)
                .map_err(|_| ENOMEM)?;
            self.heap.mapped_end += page;
        }
        self.heap.brk = new_brk;
        Ok(old)
    }

    /// Returns every owned frame, retaining this image if the allocator rejects it.
    pub fn destroy(self, allocator: &mut dyn FrameSource) -> Result<(), LoadedImageDestroyError> {
        let Self {
            address_space,
            entry,
            user_stack_top,
            heap,
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
                        heap,
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
pub fn load_image<M: FrameStore>(
    bytes: &[u8],
    allocator: &mut dyn FrameSource,
    memory: &mut M,
) -> Result<LoadedImage, LoadError<M::Error>> {
    load_image_with_kernel_mappings(bytes, allocator, memory, core::iter::empty())
}

/// Validates an ELF completely, then materializes it into an inactive address
/// space that also borrows every kernel mapping as supervisor-only pages.
///
/// The kernel mappings must be installed before the user pages so that the
/// U-mode entry, trap stack, and console stay reachable the moment `sret`
/// switches `satp`. A mapping with `U=1` is rejected before it can become a
/// borrowed leaf, so only user-image pages can be user-accessible.
pub fn load_image_with_kernel_mappings<
    S: ElfSource,
    M: FrameStore,
    I: IntoIterator<Item = KernelMapping>,
>(
    mut source: S,
    allocator: &mut dyn FrameSource,
    memory: &mut M,
    kernel_mappings: I,
) -> Result<LoadedImage, LoadError<M::Error>> {
    // headerとprogram header tableだけを先に読み、segmentへ触れる前に
    // load plan全体を検証する。tableは`MAX_PROGRAM_HEADERS`で上限がある。
    let file_len = source.len();
    let mut header_bytes = [0u8; ELF_HEADER_LEN];
    if file_len < ELF_HEADER_LEN {
        return Err(LoadError::Elf(ElfError::HeaderSize));
    }
    source
        .read_exact_at(0, &mut header_bytes)
        .map_err(|()| LoadError::SourceRead)?;
    let header = ElfHeader::parse(&header_bytes, file_len).map_err(LoadError::Elf)?;
    let mut table = vec![0u8; header.program_header_table_len()];
    source
        .read_exact_at(header.program_header_offset(), &mut table)
        .map_err(|()| LoadError::SourceRead)?;
    let image = ElfImage::from_table(header, &table).map_err(LoadError::Elf)?;
    let plan = LoadPlan::new(&image).map_err(LoadError::Elf)?;
    drop(table);
    let entry = plan.entry();
    let user_stack_top =
        VirtAddr::try_new(USER_STACK_TOP).map_err(|_| LoadError::Elf(ElfError::RangeOverflow))?;

    let mut builder = AddressSpaceBuilder::new(allocator, memory).map_err(LoadError::Vm)?;
    for mapping in kernel_mappings {
        if mapping.flags().user() {
            return Err(LoadError::UserAccessibleKernelMapping);
        }
        builder.map_borrowed_any(mapping).map_err(LoadError::Vm)?;
    }
    // segmentのbyteは1 pageずつこのbufferへ読んでからframeへcopyする。
    let mut page_buffer: Vec<u8> = vec![0u8; PAGE_SIZE];
    for segment in plan.segments() {
        materialize_segment(&mut source, &mut page_buffer, &mut builder, segment)?;
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

    let heap_start = plan.image_end();
    Ok(LoadedImage {
        address_space: builder.finish(),
        entry,
        user_stack_top,
        heap: UserHeap {
            image_pages: plan.total_user_pages(),
            start: heap_start,
            brk: heap_start,
            mapped_end: heap_start,
        },
    })
}

fn materialize_segment<S: ElfSource, M: FrameStore>(
    source_file: &mut S,
    page_buffer: &mut [u8],
    builder: &mut AddressSpaceBuilder<'_, '_, M>,
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
        // `LoadPlan`がsegmentのfile範囲を`len()`内と検証済みである。
        let source = page_buffer
            .get_mut(..copy_len)
            .ok_or(LoadError::Elf(ElfError::RangeOverflow))?;
        source_file
            .read_exact_at(source_start, source)
            .map_err(|()| LoadError::SourceRead)?;
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
            FrameStore, KernelMapPlan, KernelMapping, PageFlags, PhysAddr, VirtAddr, VirtPage,
            VmError,
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
        KernelMapPlan::new(&sections, 0x8021_5000, 0x8780_0000, 0x1000_0000).unwrap()
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

        let image =
            load_image_with_kernel_mappings(&bytes, &mut allocator, &mut memory, plan.mappings())
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

        assert!(matches!(
            load_image_with_kernel_mappings(&bytes, &mut allocator, &mut memory, [mapping],),
            Err(LoadError::UserAccessibleKernelMapping)
        ));
    }

    fn read_virtual(
        image: &LoadedImage,
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

        let image = load_image(&bytes, &mut allocator, &mut memory).unwrap();

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

        let image = load_image(&bytes, &mut allocator, &mut memory).unwrap();

        assert_eq!(allocator.stats().allocated - before.allocated, 23);
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
    }

    // Catches sbrk starting anywhere but the image end, moving the break on
    // a rejected request, mapping heap pages executable, double-mapping
    // after a partial failure, or leaking heap frames past destroy.
    #[test]
    fn sbrk_grows_from_the_image_end_and_keeps_partial_maps_on_exhaustion() {
        use minios_abi::syscall::{EINVAL, ENOMEM};

        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();
        let mut image = load_image(&bytes, &mut allocator, &mut memory).unwrap();
        let start = 0x0020_1000;

        assert_eq!(image.sbrk(0, &mut allocator, &mut memory), Ok(start));
        assert_eq!(image.sbrk(-1, &mut allocator, &mut memory), Err(EINVAL));
        assert_eq!(
            image.sbrk(isize::MAX, &mut allocator, &mut memory),
            Err(ENOMEM)
        );
        // guardより下でもimage 2 page + heapが上限を超えればENOMEM。
        let over_limit = isize::try_from(crate::elf::MAX_USER_IMAGE_PAGES * PAGE_SIZE).unwrap();
        assert_eq!(
            image.sbrk(over_limit, &mut allocator, &mut memory),
            Err(ENOMEM)
        );
        assert_eq!(image.brk(), start);

        assert_eq!(image.sbrk(10, &mut allocator, &mut memory), Ok(start));
        assert_eq!(image.brk(), start + 10);
        let heap_flags = image
            .address_space()
            .translate(&memory, VirtAddr::try_new(start).unwrap())
            .unwrap()
            .1;
        assert_eq!(heap_flags, PageFlags::new(true, true, false, true).unwrap());
        assert!(
            read_virtual(&image, &memory, start, PAGE_SIZE)
                .iter()
                .all(|byte| *byte == 0)
        );

        // 残りframeより多いpageを要求すると、map済みpageは残しbreakは動かない。
        let free = allocator.stats().free;
        let request = isize::try_from((free + 8) * PAGE_SIZE).unwrap();
        assert_eq!(
            image.sbrk(request, &mut allocator, &mut memory),
            Err(ENOMEM)
        );
        assert_eq!(image.brk(), start + 10);
        assert_eq!(allocator.stats().free, 0);

        // map済みの範囲に収まる成長はframeを確保せず成功する。
        let step = isize::try_from(PAGE_SIZE).unwrap();
        assert_eq!(
            image.sbrk(step, &mut allocator, &mut memory),
            Ok(start + 10)
        );
        assert_eq!(allocator.stats().free, 0);

        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
    }

    // Catches sbrk shrinking leaking frames, unmapping a page still below
    // the break, or letting the break fall under the heap start: shrinking
    // must return every wholly freed page to the allocator, keep the page
    // holding the new break, and remap fresh zeroed frames on regrowth.
    #[test]
    fn sbrk_shrinks_by_unmapping_freed_pages_and_regrows_zeroed() {
        use minios_abi::syscall::EINVAL;

        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let mut memory = TestFrameStore::default();
        let mut image = load_image(&bytes, &mut allocator, &mut memory).unwrap();
        let start = image.brk();
        let base = allocator.stats();
        let page = isize::try_from(PAGE_SIZE).unwrap();

        assert_eq!(image.sbrk(3 * page, &mut allocator, &mut memory), Ok(start));
        let grown = allocator.stats();
        assert!(grown.allocated >= base.allocated + 3);
        let third = start + 2 * PAGE_SIZE as u64;
        let (physical, _) = image
            .address_space()
            .translate(&memory, VirtAddr::try_new(third).unwrap())
            .unwrap();
        memory
            .copy_into(physical.as_u64() as usize, 0, b"dirty")
            .unwrap();

        // breakを2.5 page分戻すと、0.5 pageが残る先頭pageだけがmapされる。
        assert_eq!(
            image.sbrk(-(5 * page / 2), &mut allocator, &mut memory),
            Ok(start + 3 * PAGE_SIZE as u64)
        );
        assert_eq!(image.brk(), start + PAGE_SIZE as u64 / 2);
        assert_eq!(allocator.stats().allocated, grown.allocated - 2);
        assert!(
            image
                .address_space()
                .translate(&memory, VirtAddr::try_new(start).unwrap())
                .is_ok()
        );
        for gone in [start + PAGE_SIZE as u64, third] {
            assert_eq!(
                image
                    .address_space()
                    .translate(&memory, VirtAddr::try_new(gone).unwrap()),
                Err(VmError::NotMapped)
            );
        }

        // heap先頭より下へは戻せず、breakも動かない。
        assert_eq!(image.sbrk(-page, &mut allocator, &mut memory), Err(EINVAL));
        assert_eq!(image.brk(), start + PAGE_SIZE as u64 / 2);

        // 再成長したpageは新しいzero済みframeである。
        assert_eq!(
            image.sbrk(5 * page / 2, &mut allocator, &mut memory),
            Ok(start + PAGE_SIZE as u64 / 2)
        );
        assert_eq!(allocator.stats().allocated, grown.allocated);
        assert!(
            read_virtual(&image, &memory, third, PAGE_SIZE)
                .iter()
                .all(|byte| *byte == 0)
        );

        // heap先頭まで戻せばheap pageはすべて返る。
        assert_eq!(
            image.sbrk(-(3 * page), &mut allocator, &mut memory),
            Ok(start + 3 * PAGE_SIZE as u64)
        );
        assert_eq!(image.brk(), start);
        assert_eq!(allocator.stats().allocated, grown.allocated - 3);
        image.destroy(&mut allocator).unwrap();
    }

    // Catches the heap-shrink unmap reaching pages it does not own as heap:
    // stack pages and unmapped pages must be left alone and report false,
    // and a foreign allocator must be refused before anything changes.
    #[test]
    fn unmap_owned_user_skips_stack_and_unmapped_pages() {
        use crate::vm::VirtPage;

        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let mut memory = TestFrameStore::default();
        let mut image = load_image(&bytes, &mut allocator, &mut memory).unwrap();
        let stats = allocator.stats();
        let owned = image.address_space().owned_frames();

        let stack = VirtPage::from_start(USER_STACK_BOTTOM).unwrap();
        let hole = VirtPage::from_start(0x0030_0000).unwrap();
        for page in [stack, hole] {
            assert_eq!(
                image
                    .address_space_mut()
                    .unmap_owned_user(&mut allocator, &mut memory, page),
                Ok(false)
            );
        }
        let mut foreign = test_allocator::<1>(0x1000, 0x41_000);
        assert_eq!(
            image
                .address_space_mut()
                .unmap_owned_user(&mut foreign, &mut memory, stack),
            Err(VmError::OutOfFrames)
        );
        assert_eq!(allocator.stats(), stats);
        assert_eq!(image.address_space().owned_frames(), owned);
        assert!(
            image
                .address_space()
                .translate(&memory, VirtAddr::try_new(USER_STACK_BOTTOM).unwrap())
                .is_ok()
        );
        image.destroy(&mut allocator).unwrap();
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

        let image = load_image(&bytes, &mut allocator, &mut memory).unwrap();

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

        assert!(matches!(
            load_image(&bytes, &mut allocator, &mut memory),
            Err(LoadError::Elf(_))
        ));
        assert_eq!(allocator.stats(), before);
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

        assert!(matches!(
            load_image(&bytes, &mut allocator, &mut memory),
            Err(LoadError::Elf(_))
        ));
        assert_eq!(allocator.stats(), before);
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

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory)
                .err()
                .unwrap(),
            LoadError::Memory(TestStoreError::InjectedCopyFailure)
        );
        assert_eq!(allocator.stats(), before);

        memory.clear_failures();
        let image = load_image(&bytes, &mut allocator, &mut memory).unwrap();
        assert_eq!(read_virtual(&image, &memory, 0x0020_0000, 4), b"MCB1");
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
    }

    // Catches dropping the builder without recovering frames after a zeroing
    // failure that occurs after earlier page-table allocations succeeded.
    #[test]
    fn vm_zero_failure_rolls_back_every_owned_frame() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::fail_zero_after(2);

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory)
                .err()
                .unwrap(),
            LoadError::Vm(VmError::Store(TestStoreError::InjectedZeroFailure))
        );
        assert_eq!(allocator.stats(), before);
    }

    // Catches treating page-table writes as direct memory-copy failures or
    // retaining frames after a VM installation failure.
    #[test]
    fn vm_write_failure_rolls_back_every_owned_frame() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::fail_write_after(1);

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory)
                .err()
                .unwrap(),
            LoadError::Vm(VmError::Store(TestStoreError::InjectedWriteFailure))
        );
        assert_eq!(allocator.stats(), before);
    }

    // Catches the ownership ledger still behaving like a fixed-capacity
    // array: a normal image owns more frames than the old minimal arena
    // held, and every one must be returned on destroy.
    #[test]
    fn owned_frame_ledger_grows_past_the_old_arena_and_reclaims_all() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();

        let image = load_image(&bytes, &mut allocator, &mut memory).unwrap();

        assert!(image.address_space().owned_frames() > 2);
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
    }

    // Catches leaking the root or intermediate frame when physical frames run
    // out before the first user leaf can be installed.
    #[test]
    fn allocator_exhaustion_rolls_back_every_frame() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = test_allocator::<1>(0x1000, 0x3000);
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();

        assert_eq!(
            load_image(&bytes, &mut allocator, &mut memory)
                .err()
                .unwrap(),
            LoadError::Vm(VmError::OutOfFrames)
        );
        assert_eq!(allocator.stats(), before);
    }

    // Catches normal destruction retaining any frame/storage token and proves
    // a reused executable frame is overwritten by the next complete image.
    #[test]
    fn successful_destroy_restores_allocator_and_reuses_storage() {
        let bytes = fixture::valid_riscv64_elf();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();
        let entry = VirtAddr::try_new(0x0010_0000).unwrap();

        let image = load_image(&bytes, &mut allocator, &mut memory).unwrap();
        let first_text_frame = image.address_space().translate(&memory, entry).unwrap().0;
        assert_eq!(
            read_virtual(&image, &memory, 0x0010_0000, 4),
            [0x13, 0, 0, 0]
        );
        assert!(allocator.stats().allocated > 0);
        image.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);

        let mut replacement = fixture::valid_riscv64_elf();
        replacement[0x1000..0x1004].copy_from_slice(&0x0010_0073_u32.to_le_bytes());
        let second = load_image(&replacement, &mut allocator, &mut memory).unwrap();
        let second_text_frame = second.address_space().translate(&memory, entry).unwrap().0;
        assert_eq!(second_text_frame, first_text_frame);
        assert_eq!(
            read_virtual(&second, &memory, 0x0010_0000, 4),
            0x0010_0073_u32.to_le_bytes()
        );
        second.destroy(&mut allocator).unwrap();
        assert_eq!(allocator.stats(), before);
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
        let image = load_image(&bytes, &mut allocator, &mut memory).unwrap();
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
        let image = load_image(&bytes, &mut allocator, &mut memory).unwrap();
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
    }

    /// Serves an in-memory file and records every read, optionally failing
    /// the read numbered `fail_at` (0-based).
    struct RecordingSource {
        bytes: Vec<u8>,
        reads: Vec<(usize, usize)>,
        fail_at: Option<usize>,
    }

    impl RecordingSource {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                reads: Vec::new(),
                fail_at: None,
            }
        }
    }

    impl super::ElfSource for RecordingSource {
        fn len(&self) -> usize {
            self.bytes.len()
        }

        fn read_exact_at(&mut self, offset: usize, output: &mut [u8]) -> Result<(), ()> {
            if self.fail_at == Some(self.reads.len()) {
                return Err(());
            }
            self.reads.push((offset, output.len()));
            output.copy_from_slice(&self.bytes[offset..offset + output.len()]);
            Ok(())
        }
    }

    // Catches the streaming loader buffering or reading more than it maps:
    // trailing non-loadable bytes (as debug sections are) must never be
    // read, no single read may exceed one page, and the image must match the
    // one loaded from the same bytes in memory.
    #[test]
    fn streaming_source_reads_only_header_table_and_load_segments() {
        let fixture_bytes = fixture::valid_riscv64_elf();
        let mut bytes = fixture_bytes.to_vec();
        bytes.extend(core::iter::repeat_n(0xaa, 64 * 1024));
        let mut source = RecordingSource::new(bytes);
        let mut allocator = fixture_allocator();
        let mut memory = TestFrameStore::default();

        let image = load_image_with_kernel_mappings(
            &mut source,
            &mut allocator,
            &mut memory,
            core::iter::empty(),
        )
        .unwrap();

        assert_eq!(source.reads[0], (0, 64));
        assert!(
            source
                .reads
                .iter()
                .all(|&(offset, len)| len <= PAGE_SIZE && offset + len <= fixture_bytes.len())
        );
        assert_eq!(
            read_virtual(&image, &memory, 0x0010_0000, 4),
            [0x13, 0, 0, 0]
        );
        assert_eq!(read_virtual(&image, &memory, 0x0020_0000, 4), b"MCB1");
        image.destroy(&mut allocator).unwrap();
    }

    // Catches a failed source read leaking frames or being reported as an
    // ELF or memory error: a segment read failing after the header and table
    // were read must surface as SourceRead and roll back every frame.
    #[test]
    fn source_read_failure_is_reported_and_rolls_back_every_frame() {
        let bytes = fixture::valid_riscv64_elf().to_vec();
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();

        for fail_at in 0..3 {
            let mut source = RecordingSource::new(bytes.clone());
            source.fail_at = Some(fail_at);
            assert_eq!(
                load_image_with_kernel_mappings(
                    &mut source,
                    &mut allocator,
                    &mut memory,
                    core::iter::empty(),
                )
                .err()
                .unwrap(),
                LoadError::SourceRead
            );
            assert_eq!(allocator.stats(), before);
        }
    }

    // Catches an unbounded program-header table reaching the loader's
    // buffer: more than MAX_PROGRAM_HEADERS entries is rejected from the
    // file header alone, before the table is read or anything allocated.
    #[test]
    fn oversized_program_header_table_is_rejected_from_the_header() {
        use crate::elf::{ElfError, MAX_PROGRAM_HEADERS};

        let mut bytes = fixture::valid_riscv64_elf().to_vec();
        bytes[56..58].copy_from_slice(&(MAX_PROGRAM_HEADERS as u16 + 1).to_le_bytes());
        let mut source = RecordingSource::new(bytes);
        let mut allocator = fixture_allocator();
        let before = allocator.stats();
        let mut memory = TestFrameStore::default();

        assert_eq!(
            load_image_with_kernel_mappings(
                &mut source,
                &mut allocator,
                &mut memory,
                core::iter::empty(),
            )
            .err()
            .unwrap(),
            LoadError::Elf(ElfError::TooManyProgramHeaders)
        );
        assert_eq!(source.reads, [(0, 64)]);
        assert_eq!(allocator.stats(), before);
    }
}
