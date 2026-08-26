//! Fixed-capacity validation of user segment layout and permissions.

use super::header::{ElfError, ElfImage, MAX_LOAD_SEGMENTS, ProgramHeader};
use crate::vm::{PageFlags, VirtAddr, VirtPage, address::PAGE_SIZE};

/// Lowest virtual address available to a user image.
pub const USER_START: u64 = 0x0010_0000;
/// Exclusive upper bound of the user address range.
pub const USER_END: u64 = 0x4000_0000;
/// First virtual address occupied by the 64 KiB user stack.
pub const USER_STACK_BOTTOM: u64 = 0x3fff_0000;
/// Exclusive upper bound of the user stack.
pub const USER_STACK_TOP: u64 = 0x4000_0000;
/// First virtual address of the unmapped guard page.
pub const USER_GUARD_BOTTOM: u64 = 0x3ffe_f000;
/// Maximum page-rounded size of all loadable user segments.
pub const MAX_USER_IMAGE_PAGES: usize = 2048;

/// A fully validated loadable ELF segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadSegment {
    file_offset: usize,
    file_len: usize,
    virtual_start: VirtAddr,
    memory_len: usize,
    first_page: VirtPage,
    page_count: usize,
    page_offset: usize,
    flags: PageFlags,
}

impl LoadSegment {
    /// Returns the first source-byte offset in the ELF image.
    pub const fn file_offset(&self) -> usize {
        self.file_offset
    }

    /// Returns the number of source bytes copied from the ELF image.
    pub const fn file_len(&self) -> usize {
        self.file_len
    }

    /// Returns the exact, possibly unaligned, virtual segment start.
    pub const fn virtual_start(&self) -> VirtAddr {
        self.virtual_start
    }

    /// Returns the segment size in memory, including zero-filled bytes.
    pub const fn memory_len(&self) -> usize {
        self.memory_len
    }

    /// Returns the first page covered by the segment mapping.
    pub const fn first_page(&self) -> VirtPage {
        self.first_page
    }

    /// Returns the number of page-rounded pages covered by the segment.
    pub const fn page_count(&self) -> usize {
        self.page_count
    }

    /// Returns the segment start offset within its first page.
    pub const fn page_offset(&self) -> usize {
        self.page_offset
    }

    /// Returns the minimal validated user PTE permissions.
    pub const fn flags(&self) -> PageFlags {
        self.flags
    }

    fn mapped_end(&self) -> Result<u64, ElfError> {
        let page_bytes = u64::try_from(self.page_count)
            .map_err(|_| ElfError::RangeOverflow)?
            .checked_mul(PAGE_SIZE)
            .ok_or(ElfError::RangeOverflow)?;
        self.first_page
            .start()
            .as_u64()
            .checked_add(page_bytes)
            .ok_or(ElfError::RangeOverflow)
    }

    fn contains_memory_address(&self, address: u64) -> bool {
        let Some(memory_len) = u64::try_from(self.memory_len).ok() else {
            return false;
        };
        let start = self.virtual_start.as_u64();
        start
            .checked_add(memory_len)
            .is_some_and(|end| start <= address && address < end)
    }
}

/// A fixed-capacity, allocation-free plan for loading an ELF image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadPlan {
    segments: [Option<LoadSegment>; MAX_LOAD_SEGMENTS],
    len: usize,
    entry: VirtAddr,
    total_user_pages: usize,
}

impl LoadPlan {
    /// Validates every load segment before returning a materialization plan.
    pub fn new(image: &ElfImage<'_>) -> Result<Self, ElfError> {
        let mut plan = Self {
            segments: [None; MAX_LOAD_SEGMENTS],
            len: 0,
            entry: image.entry(),
            total_user_pages: 0,
        };

        for header in image.program_headers() {
            let header = header?;
            if !header.is_load() {
                continue;
            }
            if plan.len == MAX_LOAD_SEGMENTS {
                return Err(ElfError::TooManyLoadSegments);
            }
            let segment = validate_segment(image.bytes(), header)?;
            for existing in plan.segments() {
                if ranges_overlap(
                    segment.first_page.start().as_u64(),
                    segment.mapped_end()?,
                    existing.first_page.start().as_u64(),
                    existing.mapped_end()?,
                ) {
                    return Err(ElfError::PageOverlap);
                }
            }
            plan.total_user_pages = plan
                .total_user_pages
                .checked_add(segment.page_count)
                .ok_or(ElfError::ImagePageLimitExceeded)?;
            if plan.total_user_pages > MAX_USER_IMAGE_PAGES {
                return Err(ElfError::ImagePageLimitExceeded);
            }
            plan.segments[plan.len] = Some(segment);
            plan.len += 1;
        }

        let entry = plan.entry.as_u64();
        if !plan
            .segments()
            .any(|segment| segment.flags.execute() && segment.contains_memory_address(entry))
        {
            return Err(ElfError::EntryNotExecutable);
        }
        Ok(plan)
    }

    /// Returns the number of loadable segments in the plan.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the plan has no loadable segments.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the validated executable entry address.
    pub const fn entry(&self) -> VirtAddr {
        self.entry
    }

    /// Returns the total number of distinct page-rounded segment pages.
    pub const fn total_user_pages(&self) -> usize {
        self.total_user_pages
    }

    /// Iterates over validated loadable segments in program-header order.
    pub fn segments(&self) -> impl Iterator<Item = &LoadSegment> {
        self.segments[..self.len].iter().flatten()
    }
}

fn validate_segment(bytes: &[u8], header: ProgramHeader) -> Result<LoadSegment, ElfError> {
    if header.file_size() > header.memory_size() {
        return Err(ElfError::FilesLargerThanMemory);
    }
    let alignment = header.alignment();
    if alignment > 1 && !alignment.is_power_of_two() {
        return Err(ElfError::InvalidAlignment);
    }
    if alignment > 1 && header.virtual_address() % alignment != header.offset() % alignment {
        return Err(ElfError::IncongruentAddressAndOffset);
    }

    let file_end = header
        .offset()
        .checked_add(header.file_size())
        .ok_or(ElfError::RangeOverflow)?;
    let file_offset = usize::try_from(header.offset()).map_err(|_| ElfError::RangeOverflow)?;
    let file_len = usize::try_from(header.file_size()).map_err(|_| ElfError::RangeOverflow)?;
    let file_end = usize::try_from(file_end).map_err(|_| ElfError::RangeOverflow)?;
    if file_end > bytes.len() {
        return Err(ElfError::RangeOverflow);
    }

    let memory_end = header
        .virtual_address()
        .checked_add(header.memory_size())
        .ok_or(ElfError::RangeOverflow)?;
    let virtual_start =
        VirtAddr::try_new(header.virtual_address()).map_err(|_| ElfError::RangeOverflow)?;
    let memory_len = usize::try_from(header.memory_size()).map_err(|_| ElfError::RangeOverflow)?;
    if header.virtual_address() < USER_START
        || header.virtual_address() >= USER_END
        || memory_end > USER_END
    {
        return Err(ElfError::OutsideUserRange);
    }

    let first_page = VirtPage::containing(virtual_start);
    let page_start = first_page.start().as_u64();
    let page_end = if header.memory_size() == 0 {
        page_start
    } else {
        memory_end
            .checked_add(PAGE_SIZE - 1)
            .ok_or(ElfError::RangeOverflow)?
            & !(PAGE_SIZE - 1)
    };
    if ranges_overlap(page_start, page_end, USER_GUARD_BOTTOM, USER_STACK_TOP) {
        return Err(ElfError::StackCollision);
    }

    if header.writable() && header.executable() {
        return Err(ElfError::WritableExecutable);
    }
    if header.writable() && !header.readable() || !header.readable() && !header.executable() {
        return Err(ElfError::InvalidPermissions);
    }
    let flags = PageFlags::new(
        header.readable(),
        header.writable(),
        header.executable(),
        true,
    )
    .map_err(|_| ElfError::InvalidPermissions)?;

    let page_count = usize::try_from((page_end - page_start) / PAGE_SIZE)
        .map_err(|_| ElfError::RangeOverflow)?;
    let page_offset = usize::try_from(header.virtual_address() - page_start)
        .map_err(|_| ElfError::RangeOverflow)?;
    Ok(LoadSegment {
        file_offset,
        file_len,
        virtual_start,
        memory_len,
        first_page,
        page_count,
        page_offset,
        flags,
    })
}

const fn ranges_overlap(
    first_start: u64,
    first_end: u64,
    second_start: u64,
    second_end: u64,
) -> bool {
    first_start < second_end && second_start < first_end
}

#[cfg(test)]
mod tests {
    use super::{ElfError, LoadPlan, MAX_USER_IMAGE_PAGES, USER_GUARD_BOTTOM, USER_STACK_BOTTOM};
    use crate::elf::{ElfImage, fixture};

    const FIRST_HEADER: usize = 64;
    const SECOND_HEADER: usize = 120;

    #[test]
    fn plans_text_data_bss_and_entry() {
        let bytes = fixture::valid_riscv64_elf();
        let image = ElfImage::parse(&bytes).unwrap();
        let plan = LoadPlan::new(&image).unwrap();

        assert_eq!(plan.len(), 2);
        assert_eq!(plan.entry().as_u64(), 0x0010_0000);
        assert_eq!(plan.total_user_pages(), 2);
        let text = plan.segments().next().unwrap();
        assert_eq!(text.file_offset(), 0x1000);
        assert_eq!(text.file_len(), 4);
        assert_eq!(text.virtual_start().as_u64(), 0x0010_0000);
        assert_eq!(text.first_page().start().as_u64(), 0x0010_0000);
        assert_eq!(text.page_count(), 1);
        assert_eq!(text.page_offset(), 0);
        assert!(text.flags().read());
        assert!(text.flags().execute());
        assert!(text.flags().user());
        assert_eq!(plan.segments().nth(1).unwrap().memory_len(), 0x1000);
    }

    #[test]
    fn accepts_zero_and_one_as_no_alignment_requirement() {
        for alignment in [0, 1] {
            let mut bytes = fixture::valid_riscv64_elf();
            set_ph_u64(&mut bytes, FIRST_HEADER, 48, alignment);
            assert!(plan(&bytes).is_ok());
        }
    }

    #[test]
    fn rejects_files_larger_than_memory() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, FIRST_HEADER, 32, 5);
        assert_plan_error(&bytes, ElfError::FilesLargerThanMemory);
    }

    #[test]
    fn rejects_non_power_of_two_alignment() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, FIRST_HEADER, 48, 3);
        assert_plan_error(&bytes, ElfError::InvalidAlignment);
    }

    #[test]
    fn rejects_incongruent_virtual_address_and_file_offset() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, FIRST_HEADER, 16, 0x0010_0004);
        assert_plan_error(&bytes, ElfError::IncongruentAddressAndOffset);
    }

    #[test]
    fn rejects_file_range_arithmetic_overflow() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, FIRST_HEADER, 8, u64::MAX);
        set_ph_u64(&mut bytes, FIRST_HEADER, 48, 1);
        assert_plan_error(&bytes, ElfError::RangeOverflow);
    }

    #[test]
    fn rejects_file_range_past_the_input() {
        let mut bytes = fixture::valid_riscv64_elf();
        let final_byte = bytes.len() as u64 - 1;
        set_ph_u64(&mut bytes, FIRST_HEADER, 8, final_byte);
        set_ph_u64(&mut bytes, FIRST_HEADER, 48, 1);
        assert_plan_error(&bytes, ElfError::RangeOverflow);
    }

    #[test]
    fn rejects_memory_range_arithmetic_overflow() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, FIRST_HEADER, 16, 0xffff_ffff_ffff_f000);
        set_ph_u64(&mut bytes, FIRST_HEADER, 40, 0x2000);
        assert_plan_error(&bytes, ElfError::RangeOverflow);
    }

    #[test]
    fn rejects_noncanonical_segment_address() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, FIRST_HEADER, 16, 0x0000_0040_0000_0000);
        set_ph_u64(&mut bytes, FIRST_HEADER, 48, 1);
        assert_plan_error(&bytes, ElfError::RangeOverflow);
    }

    #[test]
    fn rejects_segments_below_the_user_range() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, FIRST_HEADER, 16, 0x000f_f000);
        assert_plan_error(&bytes, ElfError::OutsideUserRange);
    }

    #[test]
    fn rejects_page_rounded_segment_overlap() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, SECOND_HEADER, 16, 0x0010_0800);
        set_ph_u64(&mut bytes, SECOND_HEADER, 48, 1);
        assert_plan_error(&bytes, ElfError::PageOverlap);
    }

    #[test]
    fn rejects_writable_executable_segments() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u32(&mut bytes, SECOND_HEADER, 4, 7);
        assert_plan_error(&bytes, ElfError::WritableExecutable);
    }

    #[test]
    fn rejects_write_only_segments_before_pte_construction() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u32(&mut bytes, SECOND_HEADER, 4, 2);
        assert_plan_error(&bytes, ElfError::InvalidPermissions);
    }

    #[test]
    fn rejects_segments_without_leaf_permissions() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u32(&mut bytes, SECOND_HEADER, 4, 0);
        assert_plan_error(&bytes, ElfError::InvalidPermissions);
    }

    #[test]
    fn rejects_guard_page_collision() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, SECOND_HEADER, 16, USER_GUARD_BOTTOM);
        assert_plan_error(&bytes, ElfError::StackCollision);
    }

    #[test]
    fn rejects_user_stack_collision() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, SECOND_HEADER, 16, USER_STACK_BOTTOM);
        assert_plan_error(&bytes, ElfError::StackCollision);
    }

    #[test]
    fn accepts_exactly_the_user_image_page_limit() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(
            &mut bytes,
            SECOND_HEADER,
            40,
            (MAX_USER_IMAGE_PAGES as u64 - 1) * 4096,
        );
        assert_eq!(
            plan(&bytes).unwrap().total_user_pages(),
            MAX_USER_IMAGE_PAGES
        );
    }

    #[test]
    fn rejects_more_than_the_user_image_page_limit() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(
            &mut bytes,
            SECOND_HEADER,
            40,
            MAX_USER_IMAGE_PAGES as u64 * 4096,
        );
        assert_plan_error(&bytes, ElfError::ImagePageLimitExceeded);
    }

    #[test]
    fn accepts_entry_inside_executable_memory_beyond_file_bytes() {
        let mut bytes = fixture::valid_riscv64_elf();
        set_ph_u64(&mut bytes, FIRST_HEADER, 40, 0x1000);
        put_u64(&mut bytes, 24, 0x0010_0800);
        assert_eq!(plan(&bytes).unwrap().entry().as_u64(), 0x0010_0800);
    }

    #[test]
    fn rejects_entry_in_non_executable_segment() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u64(&mut bytes, 24, 0x0020_0000);
        assert_plan_error(&bytes, ElfError::EntryNotExecutable);
    }

    #[test]
    fn rejects_entry_at_executable_memory_end() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u64(&mut bytes, 24, 0x0010_0004);
        assert_plan_error(&bytes, ElfError::EntryNotExecutable);
    }

    fn plan(bytes: &[u8]) -> Result<LoadPlan, ElfError> {
        let image = ElfImage::parse(bytes).unwrap();
        LoadPlan::new(&image)
    }

    fn assert_plan_error(bytes: &[u8], expected: ElfError) {
        assert!(matches!(plan(bytes), Err(error) if error == expected));
    }

    fn set_ph_u32(bytes: &mut [u8], header: usize, field: usize, value: u32) {
        bytes[header + field..header + field + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_ph_u64(bytes: &mut [u8], header: usize, field: usize, value: u64) {
        put_u64(bytes, header + field, value);
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
