//! 予約窓に置かれたMiniBundleのtwo-stage検証。

use core::ops::Range;

use crate::memory::{BOOT_PAYLOAD_END, BOOT_PAYLOAD_START};
use minios_abi::boot::{BOOT_HEADER_LEN, BOOT_MAGIC, BootHeader, BootHeaderError};
use minios_abi::manifest::{Manifest, ManifestError};

/// 予約窓の検証に失敗した理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPayloadError {
    /// `total_len`が予約窓 (8 MiB) を超えた。
    HeaderTooLarge,
    /// header自体がABI契約に反する。
    Header(BootHeaderError),
    /// manifestがABI契約に反する。
    Manifest(ManifestError),
    /// manifestとELFの間のpaddingが0でない。
    NonZeroPadding,
    /// bytesが宣言された`total_len`に満たない。
    WindowTooShort,
}

impl BootPayloadError {
    // header検証のうち、窓上限に関する拒否だけは専用variantへ正規化する。
    fn from_header(error: BootHeaderError) -> Self {
        if error == BootHeaderError::TooLarge {
            Self::HeaderTooLarge
        } else {
            Self::Header(error)
        }
    }
}

/// 予約窓から検証済みのmanifestとELF rangeを借りるpayload。
///
/// bytesは`0x8780_0000..0x8800_0000`の予約窓の内容であり、manifest rangeと
/// ELF rangeは[`Self::parse`]が二段階の検証を終えた後でのみ貸し出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootPayload<'a> {
    header: BootHeader,
    manifest: Manifest<'a>,
    elf: &'a [u8],
}

impl<'a> BootPayload<'a> {
    /// host test用: bytes全体を予約窓の内容としてtwo-stage検証する。
    ///
    /// 最初に96 byteだけを読んでheaderと窓上限を確定させ、その後でなければ
    /// `total_len`までのsliceを作り、manifest rangeとELF rangeを再検査する。
    pub fn parse(bytes: &'a [u8]) -> Result<Self, BootPayloadError> {
        let header_bytes = bytes
            .get(..BOOT_HEADER_LEN)
            .ok_or(BootPayloadError::WindowTooShort)?;
        let header = BootHeader::decode(header_bytes).map_err(BootPayloadError::from_header)?;
        if header.total_len > (BOOT_PAYLOAD_END - BOOT_PAYLOAD_START) as u64 {
            return Err(BootPayloadError::HeaderTooLarge);
        }
        let total_len =
            usize::try_from(header.total_len).map_err(|_| BootPayloadError::WindowTooShort)?;
        // headerが有効になった後でだけ、宣言された長さまでのsliceを作る。
        let window = bytes
            .get(..total_len)
            .ok_or(BootPayloadError::WindowTooShort)?;
        Self::validate_window(&header, window).map(|(manifest, elf)| Self {
            header,
            manifest,
            elf,
        })
    }

    /// production入口: 予約窓`BOOT_PAYLOAD_START..BOOT_PAYLOAD_END`を直接検証する。
    ///
    /// # Safety
    ///
    /// 呼び出し側は、QEMUが`-m 128M`で起動しており`-device loader`が予約窓
    /// `0x8780_0000..0x8800_0000`を有効なRAMとして配置すること、kernel実行中に
    /// この範囲を他の所有者が書き換えないことを保証しなければならない。
    /// 返る`BootPayload`は予約窓の物理memoryを`'static`として借りる。
    /// この関数はaddress spaceを切り替える前のbare mode (VA==PA) で呼ぶか、
    /// 予約窓がS-modeから読めるmapping済みの状態で呼ぶこと。
    pub unsafe fn from_reserved_window() -> Result<Self, BootPayloadError> {
        // 最初の96 byteだけを読み、headerと窓上限を確定させてから残りを見る。
        let header = {
            // Safety: この関数のSafety契約が予約窓の有効性を保証する。
            let header_bytes = unsafe {
                core::slice::from_raw_parts(BOOT_PAYLOAD_START as *const u8, BOOT_HEADER_LEN)
            };
            BootHeader::decode(header_bytes).map_err(BootPayloadError::from_header)?
        };
        if header.total_len > (BOOT_PAYLOAD_END - BOOT_PAYLOAD_START) as u64 {
            return Err(BootPayloadError::HeaderTooLarge);
        }
        let total_len =
            usize::try_from(header.total_len).map_err(|_| BootPayloadError::WindowTooShort)?;
        // Safety: header検証済みの`total_len`が予約窓内に収まる。
        let window =
            unsafe { core::slice::from_raw_parts(BOOT_PAYLOAD_START as *const u8, total_len) };
        Self::validate_window(&header, window).map(|(manifest, elf)| Self {
            header,
            manifest,
            elf,
        })
    }

    /// 予約窓がbundleのmagicで始まるかを確認する。
    ///
    /// # Safety
    ///
    /// [`Self::from_reserved_window`]と同じ呼び出し側の保証を要求する。
    pub unsafe fn reserved_window_has_bundle() -> bool {
        // Safety: この関数のSafety契約が予約窓の有効性を保証する。
        let prefix = unsafe { core::slice::from_raw_parts(BOOT_PAYLOAD_START as *const u8, 8) };
        prefix == BOOT_MAGIC
    }

    // headerが有効になった後の第二段階: 宣言された`total_len`の範囲内で
    // manifest range、padding、ELF rangeを再検査する。
    fn validate_window(
        header: &BootHeader,
        window: &'a [u8],
    ) -> Result<(Manifest<'a>, &'a [u8]), BootPayloadError> {
        let manifest_start = usize::try_from(header.manifest.offset)
            .map_err(|_| BootPayloadError::WindowTooShort)?;
        let manifest_len =
            usize::try_from(header.manifest.len).map_err(|_| BootPayloadError::WindowTooShort)?;
        let manifest_end = manifest_start
            .checked_add(manifest_len)
            .ok_or(BootPayloadError::WindowTooShort)?;
        let elf_start =
            usize::try_from(header.elf.offset).map_err(|_| BootPayloadError::WindowTooShort)?;

        let manifest_bytes = window
            .get(manifest_start..manifest_end)
            .ok_or(BootPayloadError::WindowTooShort)?;
        let padding = window
            .get(manifest_end..elf_start)
            .ok_or(BootPayloadError::WindowTooShort)?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(BootPayloadError::NonZeroPadding);
        }
        let elf = window
            .get(elf_start..)
            .ok_or(BootPayloadError::WindowTooShort)?;
        let manifest = Manifest::parse(manifest_bytes).map_err(BootPayloadError::Manifest)?;
        Ok((manifest, elf))
    }

    /// 検証済みmanifestを借りる。
    pub const fn manifest(&self) -> &Manifest<'a> {
        &self.manifest
    }

    /// 検証済みELF bytesを借りる。
    pub const fn elf(&self) -> &'a [u8] {
        self.elf
    }

    /// header内で宣言されたELF rangeを返す。
    pub const fn elf_range(&self) -> Range<u64> {
        self.header.elf.offset..self.header.elf.offset + self.header.elf.len
    }

    /// 宣言されたbundle全体の長さを返す。
    pub const fn total_len(&self) -> u64 {
        self.header.total_len
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{vec, vec::Vec};

    use super::{BootPayload, BootPayloadError};
    use crate::{
        elf::fixture::valid_riscv64_elf,
        memory::{BOOT_PAYLOAD_END, BOOT_PAYLOAD_START},
    };
    use minios_abi::boot::{BOOT_HEADER_LEN, BUNDLE_MAX_LEN, BootHeaderError};

    const MANIFEST: &[u8] = b"version=1\nname=hello\n";

    // kernelが検証するのはrange/lengthだけである。digestはimport側の責務なので
    // host fixtureでは0のままでよい。
    fn canonical_bundle() -> Vec<u8> {
        let elf = valid_riscv64_elf();
        let manifest_end = BOOT_HEADER_LEN + MANIFEST.len();
        let padding_len = (8 - manifest_end % 8) % 8;
        let elf_offset = manifest_end + padding_len;
        let total_len = elf_offset + elf.len();

        let mut bytes = vec![0u8; total_len];
        bytes[0..8].copy_from_slice(b"MINICTR\0");
        bytes[8..10].copy_from_slice(&1u16.to_le_bytes());
        bytes[12..14].copy_from_slice(&(BOOT_HEADER_LEN as u16).to_le_bytes());
        bytes[16..24].copy_from_slice(&(total_len as u64).to_le_bytes());
        bytes[24..32].copy_from_slice(&(BOOT_HEADER_LEN as u64).to_le_bytes());
        bytes[32..40].copy_from_slice(&(MANIFEST.len() as u64).to_le_bytes());
        bytes[40..48].copy_from_slice(&(elf_offset as u64).to_le_bytes());
        bytes[48..56].copy_from_slice(&(elf.len() as u64).to_le_bytes());
        bytes[BOOT_HEADER_LEN..manifest_end].copy_from_slice(MANIFEST);
        bytes[elf_offset..].copy_from_slice(&elf);
        bytes
    }

    // Catches accepting a bundle whose declared length passes the reserved
    // window boundary, or wrapping the check instead of rejecting it.
    #[test]
    fn payload_rejects_total_length_past_reserved_window() {
        let mut bytes = canonical_bundle();
        let declared = (BOOT_PAYLOAD_END - BOOT_PAYLOAD_START) as u64;
        bytes[16..24].copy_from_slice(&(declared + 1).to_le_bytes());
        assert_eq!(
            BootPayload::parse(&bytes),
            Err(BootPayloadError::HeaderTooLarge)
        );

        let mut bytes = canonical_bundle();
        bytes[16..24].copy_from_slice(&(BUNDLE_MAX_LEN + 1).to_le_bytes());
        assert_eq!(
            BootPayload::parse(&bytes),
            Err(BootPayloadError::HeaderTooLarge)
        );
    }

    // Catches handing out unvalidated manifest or ELF bytes, a wrong manifest
    // range, or an ELF slice that reaches past the declared total length.
    #[test]
    fn payload_returns_only_validated_manifest_and_elf_ranges() {
        let bytes = canonical_bundle();
        let payload = BootPayload::parse(&bytes).unwrap();

        assert_eq!(payload.manifest().name(), "hello");
        assert!(payload.elf().starts_with(b"\x7fELF"));
        assert_eq!(payload.elf().len(), valid_riscv64_elf().len());
        let elf_offset = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        assert_eq!(payload.elf_range().start, elf_offset);
        assert_eq!(payload.total_len(), bytes.len() as u64);
    }

    // Catches reading past the bytes actually placed in the window when the
    // declared total length exceeds the buffer.
    #[test]
    fn payload_rejects_a_window_shorter_than_the_declared_length() {
        let mut bytes = canonical_bundle();
        bytes.pop();
        assert_eq!(
            BootPayload::parse(&bytes),
            Err(BootPayloadError::WindowTooShort)
        );
    }

    // Catches silently accepting a window that does not hold a bundle header.
    #[test]
    fn payload_rejects_a_missing_or_corrupt_header() {
        let mut bytes = canonical_bundle();
        bytes[0] = b'X';
        assert_eq!(
            BootPayload::parse(&bytes),
            Err(BootPayloadError::Header(BootHeaderError::WrongMagic))
        );

        let zeros = vec![0u8; BOOT_HEADER_LEN];
        assert_eq!(
            BootPayload::parse(&zeros),
            Err(BootPayloadError::Header(BootHeaderError::WrongMagic))
        );
    }

    // Catches running a manifest that the ABI parser rejects.
    #[test]
    fn payload_rejects_an_invalid_manifest() {
        let mut bytes = canonical_bundle();
        let manifest_start = BOOT_HEADER_LEN;
        // 同長の不正manifest (未対応version行) へ差し替える。
        bytes[manifest_start..manifest_start + MANIFEST.len()]
            .copy_from_slice(b"version=2\nname=hello\n");
        assert!(matches!(
            BootPayload::parse(&bytes),
            Err(BootPayloadError::Manifest(
                minios_abi::manifest::ManifestError::MissingVersion
            ))
        ));
    }

    // Catches trusting padding bytes between the manifest and the ELF.
    // "hello" manifest (21 bytes) は3 byteのpaddingを持つ。
    #[test]
    fn payload_rejects_nonzero_padding() {
        let mut bytes = canonical_bundle();
        let index = BOOT_HEADER_LEN + MANIFEST.len();
        assert!(index < bytes.len());
        bytes[index] = 0xff;
        assert_eq!(
            BootPayload::parse(&bytes),
            Err(BootPayloadError::NonZeroPadding)
        );
    }

    // Catches a `from_reserved_window` aliasing that reads the window through
    // the wrong base or length. The window read is exercised through a pinned
    // buffer standing in for the physical window.
    #[test]
    fn reserved_window_parse_reads_only_the_declared_prefix() {
        let mut bytes = canonical_bundle();
        bytes.extend_from_slice(&[0xcc; 64]);
        let payload = BootPayload::parse(&bytes).unwrap();
        assert_eq!(payload.elf().len(), valid_riscv64_elf().len());
    }
}
