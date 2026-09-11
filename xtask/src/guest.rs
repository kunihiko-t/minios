//! Rust guest (`minios-guest`) のビルドと、kernel ELF loader契約のhost検査。

use std::{fmt, path::Path, path::PathBuf, process::Command};

/// guest package名。ビルド成果物のbin名も同じである。
pub const GUEST_PACKAGE: &str = "minios-guest";
/// guest build target。
pub const GUEST_TARGET: &str = "riscv64gc-unknown-none-elf";
/// linker scriptが_startへ置くentry address (=USER_START)。
pub const GUEST_ENTRY: u64 = 0x0010_0000;

/// guestのビルドと検査に失敗した理由。
#[derive(Debug)]
pub enum GuestError {
    /// cargoがguest binaryをbuildできなかった。
    Build { status: Option<i32>, log: String },
    /// 成果物の読み取りが失敗した。
    Io(String),
}

impl fmt::Display for GuestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build { status, log } => write!(
                formatter,
                "guest build failed with status {}:\
                 {log}",
                status
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            ),
            Self::Io(message) => write!(formatter, "guest io failed: {message}"),
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

/// テスト間で共有する、ビルド済みguest ELFのbytes。
///
/// 並行testがそれぞれcargoを起動して同じ成果物を競合させないよう、
/// 一度だけbuildして読み込んだ中身をprocess全体で共有する。
#[cfg(test)]
fn guest_bytes() -> &'static [u8] {
    use std::sync::OnceLock;
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let path = build_guest().expect("guest binary must build");
        std::fs::read(&path).expect("built guest ELF must be readable")
    })
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{GUEST_ENTRY, guest_bytes};
    use minios_kernel::elf::{ElfImage, LoadPlan, USER_GUARD_BOTTOM, USER_START};

    // kernel本体のparserとload planでguestを検査する。
    // 独自parserを挟まないため、PT_INTERPやheader欄など、kernelが拒否する
    // 条件すべてがこの検査と実行時の判定で一致する。
    #[test]
    fn guest_elf_passes_the_kernel_loader_contract() {
        let bytes = guest_bytes();

        let image = ElfImage::parse(bytes).expect("kernel parser must accept the guest ELF");
        let plan = LoadPlan::new(&image).expect("kernel load plan must accept the guest ELF");

        assert_eq!(image.entry().as_u64(), GUEST_ENTRY);
        assert!(plan.segments().count() >= 1);

        for segment in plan.segments() {
            let flags = segment.flags();
            assert!(flags.read(), "every mapped page must be readable");
            assert!(
                !flags.write() || !flags.execute(),
                "no segment may be writable and executable"
            );
            let vaddr = segment.virtual_start().as_u64();
            let memory_end = vaddr + segment.memory_len() as u64;
            assert!(vaddr >= USER_START, "segment below USER_START");
            assert!(
                memory_end <= USER_GUARD_BOTTOM,
                "segment must stay below the guard page"
            );
        }
    }

    // entryが実行可能segmentの先頭pageに乗っていることはparser契約
    // (EntryNotExecutable) が保証する。ここでは文書化された配置
    // (entry=USER_START) を明示的に固定する。
    #[test]
    fn guest_entry_stays_at_the_documented_user_start() {
        let bytes = guest_bytes();
        let image = ElfImage::parse(bytes).expect("kernel parser must accept the guest ELF");
        assert_eq!(image.entry().as_u64(), USER_START);
    }
}
