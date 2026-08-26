//! Checked ELF64 header and program-header parsing.

use crate::vm::VirtAddr;

const ELF_HEADER_LEN: usize = 64;
const PROGRAM_HEADER_LEN: usize = 56;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ELF_VERSION: u32 = 1;
const ET_EXEC: u16 = 2;
const EM_RISCV: u16 = 243;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Maximum number of loadable segments accepted from one ELF image.
pub const MAX_LOAD_SEGMENTS: usize = 8;

/// A typed rejection returned while parsing or planning an ELF image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// The input does not begin with the ELF magic bytes.
    BadMagic,
    /// The ELF object is not ELF64.
    UnsupportedClass,
    /// The ELF object is not little-endian.
    UnsupportedEndian,
    /// An ELF identification or header version is unsupported.
    UnsupportedVersion,
    /// The ELF object is not a static executable.
    UnsupportedType,
    /// The ELF object does not target RISC-V.
    UnsupportedMachine,
    /// The ELF header is truncated or reports the wrong size.
    HeaderSize,
    /// Program headers do not have the ELF64 size.
    ProgramHeaderSize,
    /// The program-header table cannot be represented or is outside the input.
    ProgramHeaderTableOutOfBounds,
    /// The image has more loadable segments than fixed storage permits.
    TooManyLoadSegments,
    /// A loadable segment has more file bytes than memory bytes.
    FilesLargerThanMemory,
    /// A segment alignment is neither zero, one, nor a power of two.
    InvalidAlignment,
    /// A segment virtual address and file offset violate ELF congruence.
    IncongruentAddressAndOffset,
    /// A file, memory, address, or page-range calculation cannot be represented.
    RangeOverflow,
    /// A segment does not fit completely inside the user image range.
    OutsideUserRange,
    /// Two segment mappings overlap after rounding to pages.
    PageOverlap,
    /// A segment requests both write and execute permission.
    WritableExecutable,
    /// Segment flags cannot form a valid RISC-V leaf mapping.
    InvalidPermissions,
    /// A segment mapping intersects the guard page or user stack.
    StackCollision,
    /// The entry point is outside every executable segment memory range.
    EntryNotExecutable,
    /// Page-rounded load segments exceed the fixed image-page budget.
    ImagePageLimitExceeded,
}

/// A borrowed, validated ELF64 executable image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElfImage<'a> {
    bytes: &'a [u8],
    entry: VirtAddr,
    program_header_offset: usize,
    program_header_count: usize,
}

impl<'a> ElfImage<'a> {
    /// Parses and validates the ELF header and program-header table envelope.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ElfError> {
        if bytes.len() < ELF_HEADER_LEN {
            return Err(ElfError::HeaderSize);
        }
        if bytes.get(0..4) != Some(b"\x7fELF") {
            return Err(ElfError::BadMagic);
        }
        if bytes[4] != ELFCLASS64 {
            return Err(ElfError::UnsupportedClass);
        }
        if bytes[5] != ELFDATA2LSB {
            return Err(ElfError::UnsupportedEndian);
        }
        if bytes[6] != ELF_VERSION as u8 || read_u32(bytes, 20)? != ELF_VERSION {
            return Err(ElfError::UnsupportedVersion);
        }
        if read_u16(bytes, 16)? != ET_EXEC {
            return Err(ElfError::UnsupportedType);
        }
        if read_u16(bytes, 18)? != EM_RISCV {
            return Err(ElfError::UnsupportedMachine);
        }
        if usize::from(read_u16(bytes, 52)?) != ELF_HEADER_LEN {
            return Err(ElfError::HeaderSize);
        }
        if usize::from(read_u16(bytes, 54)?) != PROGRAM_HEADER_LEN {
            return Err(ElfError::ProgramHeaderSize);
        }

        let program_header_offset = usize::try_from(read_u64(bytes, 32)?)
            .map_err(|_| ElfError::ProgramHeaderTableOutOfBounds)?;
        let program_header_count = usize::from(read_u16(bytes, 56)?);
        let table_len = program_header_count
            .checked_mul(PROGRAM_HEADER_LEN)
            .ok_or(ElfError::ProgramHeaderTableOutOfBounds)?;
        let table_end = program_header_offset
            .checked_add(table_len)
            .ok_or(ElfError::ProgramHeaderTableOutOfBounds)?;
        if table_end > bytes.len() {
            return Err(ElfError::ProgramHeaderTableOutOfBounds);
        }

        let entry = VirtAddr::try_new(read_u64(bytes, 24)?).map_err(|_| ElfError::RangeOverflow)?;
        let image = Self {
            bytes,
            entry,
            program_header_offset,
            program_header_count,
        };
        let mut load_count = 0usize;
        for header in image.program_headers() {
            if header?.is_load() {
                load_count = load_count
                    .checked_add(1)
                    .ok_or(ElfError::TooManyLoadSegments)?;
                if load_count > MAX_LOAD_SEGMENTS {
                    return Err(ElfError::TooManyLoadSegments);
                }
            }
        }
        Ok(image)
    }

    /// Returns the executable entry address from the ELF header.
    pub const fn entry(&self) -> VirtAddr {
        self.entry
    }

    /// Returns the number of entries in the program-header table.
    pub const fn program_header_count(&self) -> usize {
        self.program_header_count
    }

    /// Iterates over program headers with checked offset and slice arithmetic.
    pub fn program_headers(&self) -> impl Iterator<Item = Result<ProgramHeader, ElfError>> + '_ {
        ProgramHeaders::new(
            self.bytes,
            self.program_header_offset,
            self.program_header_count,
        )
    }

    pub(crate) const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// The fields needed from one ELF64 program header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramHeader {
    kind: u32,
    flags: u32,
    offset: u64,
    virtual_address: u64,
    file_size: u64,
    memory_size: u64,
    alignment: u64,
}

impl ProgramHeader {
    fn parse(bytes: &[u8]) -> Result<Self, ElfError> {
        Ok(Self {
            kind: read_u32(bytes, 0)?,
            flags: read_u32(bytes, 4)?,
            offset: read_u64(bytes, 8)?,
            virtual_address: read_u64(bytes, 16)?,
            file_size: read_u64(bytes, 32)?,
            memory_size: read_u64(bytes, 40)?,
            alignment: read_u64(bytes, 48)?,
        })
    }

    /// Returns whether this header describes a `PT_LOAD` segment.
    pub const fn is_load(self) -> bool {
        self.kind == PT_LOAD
    }

    /// Returns the file offset of this segment.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns the virtual start address of this segment.
    pub const fn virtual_address(self) -> u64 {
        self.virtual_address
    }

    /// Returns the number of bytes stored in the ELF file.
    pub const fn file_size(self) -> u64 {
        self.file_size
    }

    /// Returns the number of bytes occupied in memory.
    pub const fn memory_size(self) -> u64 {
        self.memory_size
    }

    /// Returns the ELF alignment requirement.
    pub const fn alignment(self) -> u64 {
        self.alignment
    }

    /// Returns whether the segment requests read permission.
    pub const fn readable(self) -> bool {
        self.flags & PF_R != 0
    }

    /// Returns whether the segment requests write permission.
    pub const fn writable(self) -> bool {
        self.flags & PF_W != 0
    }

    /// Returns whether the segment requests execute permission.
    pub const fn executable(self) -> bool {
        self.flags & PF_X != 0
    }
}

struct ProgramHeaders<'a> {
    bytes: &'a [u8],
    next_offset: usize,
    remaining: usize,
}

impl<'a> ProgramHeaders<'a> {
    const fn new(bytes: &'a [u8], offset: usize, count: usize) -> Self {
        Self {
            bytes,
            next_offset: offset,
            remaining: count,
        }
    }

    fn fail(&mut self) -> Option<Result<ProgramHeader, ElfError>> {
        self.remaining = 0;
        Some(Err(ElfError::ProgramHeaderTableOutOfBounds))
    }
}

impl Iterator for ProgramHeaders<'_> {
    type Item = Result<ProgramHeader, ElfError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let Some(end) = self.next_offset.checked_add(PROGRAM_HEADER_LEN) else {
            return self.fail();
        };
        let Some(bytes) = self.bytes.get(self.next_offset..end) else {
            return self.fail();
        };
        self.next_offset = end;
        self.remaining -= 1;
        Some(ProgramHeader::parse(bytes))
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ElfError> {
    let value = read_array::<2>(bytes, offset)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ElfError> {
    let value = read_array::<4>(bytes, offset)?;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ElfError> {
    let value = read_array::<8>(bytes, offset)?;
    Ok(u64::from_le_bytes(value))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], ElfError> {
    let end = offset
        .checked_add(N)
        .ok_or(ElfError::ProgramHeaderTableOutOfBounds)?;
    bytes
        .get(offset..end)
        .ok_or(ElfError::ProgramHeaderTableOutOfBounds)?
        .try_into()
        .map_err(|_| ElfError::ProgramHeaderTableOutOfBounds)
}

#[cfg(test)]
mod tests {
    use super::{ElfError, ElfImage, ProgramHeaders};
    use crate::elf::fixture;

    #[test]
    fn parses_a_static_riscv64_elf_header() {
        let bytes = fixture::valid_riscv64_elf();
        let image = ElfImage::parse(&bytes).unwrap();
        assert_eq!(image.entry().as_u64(), 0x0010_0000);
        assert_eq!(image.program_header_count(), 2);
    }

    #[test]
    fn rejects_bad_magic_byte_zero() {
        assert_header_byte_error(0, 0, ElfError::BadMagic);
    }

    #[test]
    fn rejects_bad_magic_byte_one() {
        assert_header_byte_error(1, 0, ElfError::BadMagic);
    }

    #[test]
    fn rejects_bad_magic_byte_two() {
        assert_header_byte_error(2, 0, ElfError::BadMagic);
    }

    #[test]
    fn rejects_bad_magic_byte_three() {
        assert_header_byte_error(3, 0, ElfError::BadMagic);
    }

    #[test]
    fn rejects_an_elf_header_shorter_than_sixty_four_bytes() {
        let bytes = [0u8; 63];
        assert_eq!(ElfImage::parse(&bytes), Err(ElfError::HeaderSize));
    }

    #[test]
    fn rejects_non_elf64_class() {
        assert_header_byte_error(4, 1, ElfError::UnsupportedClass);
    }

    #[test]
    fn rejects_non_little_endian_data() {
        assert_header_byte_error(5, 2, ElfError::UnsupportedEndian);
    }

    #[test]
    fn rejects_unsupported_ident_version() {
        assert_header_byte_error(6, 2, ElfError::UnsupportedVersion);
    }

    #[test]
    fn rejects_unsupported_header_version() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u32(&mut bytes, 20, 2);
        assert_eq!(ElfImage::parse(&bytes), Err(ElfError::UnsupportedVersion));
    }

    #[test]
    fn rejects_non_executable_type() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u16(&mut bytes, 16, 3);
        assert_eq!(ElfImage::parse(&bytes), Err(ElfError::UnsupportedType));
    }

    #[test]
    fn rejects_non_riscv_machine() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u16(&mut bytes, 18, 62);
        assert_eq!(ElfImage::parse(&bytes), Err(ElfError::UnsupportedMachine));
    }

    #[test]
    fn rejects_wrong_elf_header_size() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u16(&mut bytes, 52, 63);
        assert_eq!(ElfImage::parse(&bytes), Err(ElfError::HeaderSize));
    }

    #[test]
    fn rejects_wrong_program_header_size() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u16(&mut bytes, 54, 55);
        assert_eq!(ElfImage::parse(&bytes), Err(ElfError::ProgramHeaderSize));
    }

    #[test]
    fn rejects_program_header_table_past_the_input() {
        let mut bytes = fixture::valid_riscv64_elf();
        let truncated_table_offset = bytes.len() as u64 - 55;
        put_u64(&mut bytes, 32, truncated_table_offset);
        assert_eq!(
            ElfImage::parse(&bytes),
            Err(ElfError::ProgramHeaderTableOutOfBounds)
        );
    }

    #[test]
    fn rejects_program_header_table_arithmetic_overflow() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u64(&mut bytes, 32, u64::MAX);
        assert_eq!(
            ElfImage::parse(&bytes),
            Err(ElfError::ProgramHeaderTableOutOfBounds)
        );
    }

    #[test]
    fn rejects_more_than_eight_empty_load_segments() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u16(&mut bytes, 56, 9);
        for index in 2..9 {
            let destination = 64 + index * 56;
            let source = bytes[64..120].to_vec();
            bytes[destination..destination + 56].copy_from_slice(&source);
        }
        for index in 0..9 {
            put_u64(&mut bytes, 64 + index * 56 + 32, 0);
            put_u64(&mut bytes, 64 + index * 56 + 40, 0);
        }
        assert_eq!(ElfImage::parse(&bytes), Err(ElfError::TooManyLoadSegments));
    }

    #[test]
    fn accepts_exactly_eight_load_segments() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u16(&mut bytes, 56, 8);
        for index in 2..8 {
            copy_first_program_header(&mut bytes, index);
        }
        assert_eq!(ElfImage::parse(&bytes).unwrap().program_header_count(), 8);
    }

    #[test]
    fn does_not_count_non_load_program_headers_toward_the_load_limit() {
        let mut bytes = fixture::valid_riscv64_elf();
        put_u16(&mut bytes, 56, 9);
        for index in 2..9 {
            copy_first_program_header(&mut bytes, index);
        }
        for index in 0..9 {
            put_u32(&mut bytes, 64 + index * 56, 0);
        }
        assert_eq!(ElfImage::parse(&bytes).unwrap().program_header_count(), 9);
    }

    #[test]
    fn accepts_a_program_header_table_ending_at_the_input_boundary() {
        let fixture = fixture::valid_riscv64_elf();
        let mut bytes = [0u8; 176];
        bytes[..176].copy_from_slice(&fixture[..176]);
        assert_eq!(ElfImage::parse(&bytes).unwrap().program_header_count(), 2);
    }

    #[test]
    fn checked_program_header_iterator_reports_truncation_without_panicking() {
        let bytes = [0u8; 55];
        let mut headers = ProgramHeaders::new(&bytes, 0, 1);
        assert_eq!(
            headers.next(),
            Some(Err(ElfError::ProgramHeaderTableOutOfBounds))
        );
        assert_eq!(headers.next(), None);
    }

    #[test]
    fn checked_program_header_iterator_reports_offset_overflow_without_panicking() {
        let bytes = [0u8; 56];
        let mut headers = ProgramHeaders::new(&bytes, usize::MAX, 1);
        assert_eq!(
            headers.next(),
            Some(Err(ElfError::ProgramHeaderTableOutOfBounds))
        );
        assert_eq!(headers.next(), None);
    }

    fn assert_header_byte_error(offset: usize, value: u8, expected: ElfError) {
        let mut bytes = fixture::valid_riscv64_elf();
        bytes[offset] = value;
        assert_eq!(ElfImage::parse(&bytes), Err(expected));
    }

    fn copy_first_program_header(bytes: &mut [u8], index: usize) {
        let source: [u8; 56] = bytes[64..120].try_into().unwrap();
        let destination = 64 + index * 56;
        bytes[destination..destination + 56].copy_from_slice(&source);
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
