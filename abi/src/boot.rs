pub const BOOT_MAGIC: [u8; 8] = *b"MINICTR\0";
pub const BOOT_ABI_MAJOR: u16 = 1;
pub const BOOT_ABI_MINOR: u16 = 0;
pub const BOOT_HEADER_LEN: usize = 96;
pub const BUNDLE_MAX_LEN: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootHeader {
    pub total_len: u64,
    pub manifest: ByteRange,
    pub elf: ByteRange,
    pub digest: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootHeaderError {
    WrongLength,
    WrongMagic,
    UnsupportedMajor,
    UnsupportedMinor,
    WrongHeaderLength,
    NonZeroFlags,
    NonZeroReserved,
    TooLarge,
    ManifestNotAtHeaderEnd,
    ElfNotAligned,
    RangeOverflow,
    RangesOverlap,
    ElfOffsetMismatch,
    TotalLengthMismatch,
}

impl BootHeader {
    pub fn decode(bytes: &[u8]) -> Result<Self, BootHeaderError> {
        if bytes.len() != BOOT_HEADER_LEN {
            return Err(BootHeaderError::WrongLength);
        }
        if bytes.get(0..8) != Some(BOOT_MAGIC.as_slice()) {
            return Err(BootHeaderError::WrongMagic);
        }
        if read_u16(bytes, 8)? != BOOT_ABI_MAJOR {
            return Err(BootHeaderError::UnsupportedMajor);
        }
        if read_u16(bytes, 10)? != BOOT_ABI_MINOR {
            return Err(BootHeaderError::UnsupportedMinor);
        }
        if read_u16(bytes, 12)? != BOOT_HEADER_LEN as u16 {
            return Err(BootHeaderError::WrongHeaderLength);
        }
        if read_u16(bytes, 14)? != 0 {
            return Err(BootHeaderError::NonZeroFlags);
        }
        if bytes
            .get(88..96)
            .ok_or(BootHeaderError::WrongLength)?
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(BootHeaderError::NonZeroReserved);
        }

        let header = Self {
            total_len: read_u64(bytes, 16)?,
            manifest: ByteRange {
                offset: read_u64(bytes, 24)?,
                len: read_u64(bytes, 32)?,
            },
            elf: ByteRange {
                offset: read_u64(bytes, 40)?,
                len: read_u64(bytes, 48)?,
            },
            digest: bytes
                .get(56..88)
                .ok_or(BootHeaderError::WrongLength)?
                .try_into()
                .map_err(|_| BootHeaderError::WrongLength)?,
        };
        header.validate_layout()?;
        Ok(header)
    }

    pub fn encode(self) -> [u8; BOOT_HEADER_LEN] {
        let mut bytes = [0_u8; BOOT_HEADER_LEN];
        bytes[0..8].copy_from_slice(&BOOT_MAGIC);
        bytes[8..10].copy_from_slice(&BOOT_ABI_MAJOR.to_le_bytes());
        bytes[10..12].copy_from_slice(&BOOT_ABI_MINOR.to_le_bytes());
        bytes[12..14].copy_from_slice(&(BOOT_HEADER_LEN as u16).to_le_bytes());
        bytes[16..24].copy_from_slice(&self.total_len.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.manifest.offset.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.manifest.len.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.elf.offset.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.elf.len.to_le_bytes());
        bytes[56..88].copy_from_slice(&self.digest);
        bytes
    }

    pub fn encode_with_zero_digest(self) -> [u8; BOOT_HEADER_LEN] {
        Self {
            digest: [0; 32],
            ..self
        }
        .encode()
    }

    pub fn validate_layout(self) -> Result<(), BootHeaderError> {
        if self.total_len > BUNDLE_MAX_LEN {
            return Err(BootHeaderError::TooLarge);
        }
        if self.manifest.offset != BOOT_HEADER_LEN as u64 {
            return Err(BootHeaderError::ManifestNotAtHeaderEnd);
        }

        let manifest_end = self
            .manifest
            .offset
            .checked_add(self.manifest.len)
            .ok_or(BootHeaderError::RangeOverflow)?;

        if !self.elf.offset.is_multiple_of(8) {
            return Err(BootHeaderError::ElfNotAligned);
        }

        let elf_end = self
            .elf
            .offset
            .checked_add(self.elf.len)
            .ok_or(BootHeaderError::RangeOverflow)?;

        if manifest_end > self.elf.offset {
            return Err(BootHeaderError::RangesOverlap);
        }

        let padding_len = (8 - manifest_end % 8) % 8;
        let expected_elf_offset = manifest_end
            .checked_add(padding_len)
            .ok_or(BootHeaderError::RangeOverflow)?;
        if self.elf.offset != expected_elf_offset {
            return Err(BootHeaderError::ElfOffsetMismatch);
        }
        if elf_end != self.total_len {
            return Err(BootHeaderError::TotalLengthMismatch);
        }

        Ok(())
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, BootHeaderError> {
    let end = offset
        .checked_add(size_of::<u16>())
        .ok_or(BootHeaderError::WrongLength)?;
    let value = bytes
        .get(offset..end)
        .ok_or(BootHeaderError::WrongLength)?
        .try_into()
        .map_err(|_| BootHeaderError::WrongLength)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, BootHeaderError> {
    let end = offset
        .checked_add(size_of::<u64>())
        .ok_or(BootHeaderError::WrongLength)?;
    let value = bytes
        .get(offset..end)
        .ok_or(BootHeaderError::WrongLength)?
        .try_into()
        .map_err(|_| BootHeaderError::WrongLength)?;
    Ok(u64::from_le_bytes(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_bytes() -> [u8; BOOT_HEADER_LEN] {
        let mut bytes = [0_u8; BOOT_HEADER_LEN];
        bytes[0..8].copy_from_slice(b"MINICTR\0");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        bytes[12..14].copy_from_slice(&96_u16.to_le_bytes());
        bytes[16..24].copy_from_slice(&120_u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&96_u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&8_u64.to_le_bytes());
        bytes[40..48].copy_from_slice(&104_u64.to_le_bytes());
        bytes[48..56].copy_from_slice(&16_u64.to_le_bytes());
        bytes
    }

    fn canonical_header() -> BootHeader {
        BootHeader {
            total_len: 120,
            manifest: ByteRange { offset: 96, len: 8 },
            elf: ByteRange {
                offset: 104,
                len: 16,
            },
            digest: [0; 32],
        }
    }

    #[test]
    fn decodes_canonical_v1_header() {
        let mut bytes = [0_u8; BOOT_HEADER_LEN];
        bytes[0..8].copy_from_slice(b"MINICTR\0");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        bytes[12..14].copy_from_slice(&96_u16.to_le_bytes());
        bytes[16..24].copy_from_slice(&120_u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&96_u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&8_u64.to_le_bytes());
        bytes[40..48].copy_from_slice(&104_u64.to_le_bytes());
        bytes[48..56].copy_from_slice(&16_u64.to_le_bytes());
        bytes[56..88].fill(0x5a);

        let header = BootHeader::decode(&bytes).unwrap();

        assert_eq!(header.total_len, 120);
        assert_eq!(header.manifest, ByteRange { offset: 96, len: 8 });
        assert_eq!(
            header.elf,
            ByteRange {
                offset: 104,
                len: 16
            }
        );
        assert_eq!(header.digest, [0x5a; 32]);
    }

    #[test]
    fn encodes_canonical_v1_header() {
        let header = BootHeader {
            total_len: 120,
            manifest: ByteRange { offset: 96, len: 8 },
            elf: ByteRange {
                offset: 104,
                len: 16,
            },
            digest: [0xa5; 32],
        };

        let bytes = header.encode();

        assert_eq!(
            &bytes[..56],
            &[
                b'M', b'I', b'N', b'I', b'C', b'T', b'R', 0, 1, 0, 0, 0, 96, 0, 0, 0, 120, 0, 0, 0,
                0, 0, 0, 0, 96, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 104, 0, 0, 0, 0, 0, 0,
                0, 16, 0, 0, 0, 0, 0, 0, 0,
            ],
        );
        assert_eq!(&bytes[56..88], &[0xa5; 32]);
        assert_eq!(&bytes[88..96], &[0; 8]);
    }

    #[test]
    fn zero_digest_encoding_only_zeroes_the_digest_field() {
        let header = BootHeader {
            total_len: 120,
            manifest: ByteRange { offset: 96, len: 8 },
            elf: ByteRange {
                offset: 104,
                len: 16,
            },
            digest: [0xa5; 32],
        };

        let encoded = header.encode();
        let zeroed = header.encode_with_zero_digest();

        assert_eq!(&zeroed[..56], &encoded[..56]);
        assert_eq!(&zeroed[56..88], &[0; 32]);
        assert_eq!(&zeroed[88..], &encoded[88..]);
    }

    #[test]
    fn rejects_headers_that_are_not_exactly_96_bytes() {
        let short = [0_u8; BOOT_HEADER_LEN - 1];
        let long = [0_u8; BOOT_HEADER_LEN + 1];

        for (case, bytes) in [("short", &short[..]), ("long", &long[..])] {
            assert_eq!(
                BootHeader::decode(bytes),
                Err(BootHeaderError::WrongLength),
                "{case}"
            );
        }
    }

    #[test]
    fn rejects_malformed_fixed_header_fields() {
        let mut wrong_magic = canonical_bytes();
        wrong_magic[0] = b'X';

        let mut unsupported_major = canonical_bytes();
        unsupported_major[8..10].copy_from_slice(&2_u16.to_le_bytes());

        let mut unsupported_minor = canonical_bytes();
        unsupported_minor[10..12].copy_from_slice(&1_u16.to_le_bytes());

        let mut wrong_header_len = canonical_bytes();
        wrong_header_len[12..14].copy_from_slice(&95_u16.to_le_bytes());

        let mut non_zero_flags = canonical_bytes();
        non_zero_flags[14..16].copy_from_slice(&1_u16.to_le_bytes());

        let mut non_zero_reserved = canonical_bytes();
        non_zero_reserved[95] = 1;

        let cases = [
            ("WrongMagic", wrong_magic, BootHeaderError::WrongMagic),
            (
                "UnsupportedMajor",
                unsupported_major,
                BootHeaderError::UnsupportedMajor,
            ),
            (
                "UnsupportedMinor",
                unsupported_minor,
                BootHeaderError::UnsupportedMinor,
            ),
            (
                "WrongHeaderLength",
                wrong_header_len,
                BootHeaderError::WrongHeaderLength,
            ),
            (
                "NonZeroFlags",
                non_zero_flags,
                BootHeaderError::NonZeroFlags,
            ),
            (
                "NonZeroReserved",
                non_zero_reserved,
                BootHeaderError::NonZeroReserved,
            ),
        ];

        for (case, bytes, expected) in cases {
            assert_eq!(BootHeader::decode(&bytes), Err(expected), "{case}");
        }
    }

    #[test]
    fn decode_rejects_an_invalid_range_layout() {
        let mut bytes = canonical_bytes();
        bytes[16..24].copy_from_slice(&121_u64.to_le_bytes());

        assert_eq!(
            BootHeader::decode(&bytes),
            Err(BootHeaderError::TotalLengthMismatch)
        );
    }

    #[test]
    fn accepts_required_padding_and_the_maximum_bundle_length() {
        let cases = [
            (
                "one padding byte",
                BootHeader {
                    manifest: ByteRange { offset: 96, len: 7 },
                    ..canonical_header()
                },
            ),
            (
                "maximum bundle length",
                BootHeader {
                    total_len: BUNDLE_MAX_LEN,
                    elf: ByteRange {
                        offset: 104,
                        len: BUNDLE_MAX_LEN - 104,
                    },
                    ..canonical_header()
                },
            ),
        ];

        for (case, header) in cases {
            assert_eq!(header.validate_layout(), Ok(()), "{case}");
            assert_eq!(BootHeader::decode(&header.encode()), Ok(header), "{case}");
        }
    }

    #[test]
    fn rejects_invalid_layouts() {
        let cases = [
            (
                "TooLarge",
                BootHeader {
                    total_len: BUNDLE_MAX_LEN + 1,
                    ..canonical_header()
                },
                BootHeaderError::TooLarge,
            ),
            (
                "ManifestNotAtHeaderEnd",
                BootHeader {
                    manifest: ByteRange {
                        offset: 104,
                        len: 8,
                    },
                    ..canonical_header()
                },
                BootHeaderError::ManifestNotAtHeaderEnd,
            ),
            (
                "ElfNotAligned",
                BootHeader {
                    elf: ByteRange {
                        offset: 103,
                        len: 17,
                    },
                    ..canonical_header()
                },
                BootHeaderError::ElfNotAligned,
            ),
            (
                "RangeOverflow",
                BootHeader {
                    total_len: BUNDLE_MAX_LEN,
                    elf: ByteRange {
                        offset: u64::MAX - 7,
                        len: 16,
                    },
                    ..canonical_header()
                },
                BootHeaderError::RangeOverflow,
            ),
            (
                "ManifestRangeOverflow",
                BootHeader {
                    total_len: BUNDLE_MAX_LEN,
                    manifest: ByteRange {
                        offset: 96,
                        len: u64::MAX,
                    },
                    ..canonical_header()
                },
                BootHeaderError::RangeOverflow,
            ),
            (
                "RangesOverlap",
                BootHeader {
                    manifest: ByteRange {
                        offset: 96,
                        len: 16,
                    },
                    ..canonical_header()
                },
                BootHeaderError::RangesOverlap,
            ),
            (
                "ElfOffsetMismatch",
                BootHeader {
                    elf: ByteRange {
                        offset: 112,
                        len: 8,
                    },
                    ..canonical_header()
                },
                BootHeaderError::ElfOffsetMismatch,
            ),
            (
                "TotalLengthMismatch",
                BootHeader {
                    total_len: 121,
                    ..canonical_header()
                },
                BootHeaderError::TotalLengthMismatch,
            ),
        ];

        for (case, header, expected) in cases {
            assert_eq!(header.validate_layout(), Err(expected), "{case}");
            assert_eq!(
                BootHeader::decode(&header.encode()),
                Err(expected),
                "{case} bytes"
            );
        }
    }

    #[test]
    fn rejects_elf_range_overflow() {
        let header = BootHeader {
            total_len: BUNDLE_MAX_LEN,
            manifest: ByteRange { offset: 96, len: 8 },
            elf: ByteRange {
                offset: u64::MAX - 7,
                len: 16,
            },
            digest: [0; 32],
        };

        assert_eq!(
            header.validate_layout(),
            Err(BootHeaderError::RangeOverflow)
        );
    }
}
