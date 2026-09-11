//! Rust guest (`minios-guest`) のビルドと、kernel ELF loader契約のhost検査。

//! Rust guest (`minios-guest`) のビルドと、kernel ELF loader契約のhost検査。

use std::{fmt, path::PathBuf, process::Command};

/// guest package名。ビルド成果物のbin名も同じである。
pub const GUEST_PACKAGE: &str = "minios-guest";
/// guest build target。
pub const GUEST_TARGET: &str = "riscv64gc-unknown-none-elf";
/// linker scriptが_startへ置くentry address (=USER_START)。
pub const GUEST_ENTRY: u64 = 0x0010_0000;

const ELF_HEADER_LEN: usize = 64;
const PROGRAM_HEADER_LEN: usize = 56;
const PT_LOAD: u32 = 1;

/// guestのビルドと検査に失敗した理由。
#[derive(Debug)]
pub enum GuestError {
    /// cargoがguest binaryをbuildできなかった。
    Build { status: Option<i32>, log: String },
    /// 成果物の読み取りが失敗した。
    Io(String),
    /// ELF64 envelopeが静的RISC-V実行fileの契約を満たさない。
    Malformed(&'static str),
}

impl fmt::Display for GuestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build { status, log } => write!(
                formatter,
                "guest build failed with status {}:                 {log}",
                status
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            ),
            Self::Io(message) => write!(formatter, "guest io failed: {message}"),
            Self::Malformed(reason) => {
                write!(formatter, "guest ELF is malformed: {reason}")
            }
        }
    }
}

impl std::error::Error for GuestError {}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be in the workspace root")
        .to_path_buf()
}

/// guest binaryをrelease profileでbuildし、そのpathを返す。
pub fn build_guest() -> Result<PathBuf, GuestError> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace_root())
        .args([
            "build",
            "-p",
            GUEST_PACKAGE,
            "--target",
            GUEST_TARGET,
            "--release",
            "--locked",
        ])
        .output()
        .map_err(|error| GuestError::Io(error.to_string()))?;
    if !output.status.success() {
        return Err(GuestError::Build {
            status: output.status.code(),
            log: format!(
                "\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    Ok(workspace_root()
        .join("target")
        .join(GUEST_TARGET)
        .join("release")
        .join(GUEST_PACKAGE))
}

/// 検査に必要なELF64欄だけを抜いた最小model。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestSegment {
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    pub align: u64,
}

/// guest ELFの envelope と PT_LOAD 一覧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestElf {
    pub elf_type: u16,
    pub machine: u16,
    pub entry: u64,
    pub segments: Vec<GuestSegment>,
}

/// 静的RISC-V ELF64実行fileとして欄を読む。検査は呼び出し側が行う。
pub fn parse_guest_elf(bytes: &[u8]) -> Result<GuestElf, GuestError> {
    let header = bytes
        .get(..ELF_HEADER_LEN)
        .ok_or(GuestError::Malformed("header is shorter than 64 bytes"))?;
    if &header[0..4] != b"\x7fELF" {
        return Err(GuestError::Malformed("bad magic"));
    }
    if header[4] != 2 || header[5] != 1 {
        return Err(GuestError::Malformed("not ELF64 little endian"));
    }
    let elf_type = u16::from_le_bytes([header[16], header[17]]);
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let entry = u64::from_le_bytes(header[24..32].try_into().expect("fixed slice"));
    let phoff = u64::from_le_bytes(header[32..40].try_into().expect("fixed slice"));
    let phentsize = u16::from_le_bytes([header[54], header[55]]) as usize;
    let phnum = u16::from_le_bytes([header[56], header[57]]) as usize;
    if phentsize != PROGRAM_HEADER_LEN {
        return Err(GuestError::Malformed("program header size is not 56"));
    }

    let mut segments = Vec::with_capacity(phnum);
    for index in 0..phnum {
        let start = phoff as usize + index * phentsize;
        let ph = bytes
            .get(start..start + PROGRAM_HEADER_LEN)
            .ok_or(GuestError::Malformed("program header table is truncated"))?;
        let kind = u32::from_le_bytes(ph[0..4].try_into().expect("fixed slice"));
        if kind != PT_LOAD {
            continue;
        }
        segments.push(GuestSegment {
            flags: u32::from_le_bytes(ph[4..8].try_into().expect("fixed slice")),
            offset: u64::from_le_bytes(ph[8..16].try_into().expect("fixed slice")),
            vaddr: u64::from_le_bytes(ph[16..24].try_into().expect("fixed slice")),
            filesz: u64::from_le_bytes(ph[32..40].try_into().expect("fixed slice")),
            memsz: u64::from_le_bytes(ph[40..48].try_into().expect("fixed slice")),
            align: u64::from_le_bytes(ph[48..56].try_into().expect("fixed slice")),
        });
    }

    Ok(GuestElf {
        elf_type,
        machine,
        entry,
        segments,
    })
}

use std::path::Path;

#[cfg(test)]
mod tests {
    extern crate std;

    use std::fs;

    use super::{GuestElf, build_guest, parse_guest_elf};
    use minios_kernel::elf::{MAX_LOAD_SEGMENTS, USER_GUARD_BOTTOM, USER_START};

    const PF_X: u32 = 1;
    const PF_W: u32 = 2;
    const PF_R: u32 = 4;

    fn built_guest_elf() -> GuestElf {
        let path = build_guest().expect("guest binary must build");
        let bytes = fs::read(&path).expect("built guest ELF must be readable");
        parse_guest_elf(&bytes).expect("built guest ELF must parse")
    }

    // Catches a linker script that drifts away from the loader's contract:
    // wrong entry address, segments outside the user range, W+X, missing
    // read, or an entry outside every executable segment.
    #[test]
    fn guest_elf_meets_the_loader_contract() {
        let elf = built_guest_elf();

        assert_eq!(
            elf.entry,
            super::GUEST_ENTRY,
            "entry must stay at USER_START"
        );
        assert!(
            (1..=MAX_LOAD_SEGMENTS).contains(&elf.segments.len()),
            "segment count must stay inside the loader limit"
        );

        let mut executable_covers_entry = false;
        for segment in &elf.segments {
            assert!(segment.flags & PF_R != 0, "every segment must be readable");
            assert!(
                segment.flags & PF_W == 0 || segment.flags & PF_X == 0,
                "no segment may be writable and executable"
            );
            assert!(segment.filesz <= segment.memsz);
            assert!(segment.vaddr >= USER_START, "segment below USER_START");
            assert!(
                segment.vaddr + segment.memsz <= USER_GUARD_BOTTOM,
                "segment must stay below the guard page"
            );
            if segment.align > 1 {
                assert_eq!(
                    segment.vaddr % segment.align,
                    segment.offset % segment.align,
                    "file offset and vaddr must stay congruent"
                );
            }
            if segment.flags & PF_X != 0
                && elf.entry >= segment.vaddr
                && elf.entry < segment.vaddr + segment.memsz
            {
                executable_covers_entry = true;
            }
        }
        assert!(
            executable_covers_entry,
            "entry must land inside an executable segment"
        );
    }

    // Catches a broken minimal ELF64 envelope before the loader ever sees it.
    #[test]
    fn guest_elf_envelope_is_static_riscv64() {
        let path = build_guest().expect("guest binary must build");
        let bytes = fs::read(&path).expect("built guest ELF must be readable");
        let elf = parse_guest_elf(&bytes).expect("built guest ELF must parse");

        assert_eq!(&bytes[0..4], b"\x7fELF");
        assert_eq!(bytes[4], 2, "ELFCLASS64");
        assert_eq!(bytes[5], 1, "little endian");
        assert_eq!(elf.elf_type, 2, "ET_EXEC");
        assert_eq!(elf.machine, 243, "EM_RISC_V");
    }
}
