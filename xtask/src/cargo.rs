use std::{
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
};

const KERNEL_PACKAGE: &str = "minios-kernel";
const KERNEL_BINARY: &str = "minios-kernel";
const RISCV_TARGET: &str = "riscv64gc-unknown-none-elf";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoError {
    Spawn {
        command: String,
        error: String,
    },
    Failed {
        command: String,
        status: Option<i32>,
        output: String,
    },
}

impl fmt::Display for CargoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { command, error } => {
                write!(formatter, "could not start {command}: {error}")
            }
            Self::Failed {
                command,
                status,
                output,
            } => write!(
                formatter,
                "{command} failed with status {}:\n{}",
                status
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                output.trim_end()
            ),
        }
    }
}

impl std::error::Error for CargoError {}

pub fn build_kernel(qemu_test_boot: bool) -> Result<PathBuf, CargoError> {
    build_kernel_with_feature(qemu_test_boot.then_some("qemu-test-boot"))
}

pub fn build_kernel_for_test(feature: &str) -> Result<PathBuf, CargoError> {
    build_kernel_with_feature(Some(feature))
}

fn build_kernel_with_feature(feature: Option<&str>) -> Result<PathBuf, CargoError> {
    let workspace = workspace_root();
    let mut args = vec![
        "build",
        "-p",
        KERNEL_PACKAGE,
        "--bin",
        KERNEL_BINARY,
        "--target",
        RISCV_TARGET,
    ];
    if let Some(feature) = feature {
        args.extend(["--features", feature]);
    }

    run(&args)?;
    Ok(kernel_binary_path(&workspace))
}

pub fn run(args: &[&str]) -> Result<String, CargoError> {
    let command_name = format!("cargo {}", args.join(" "));
    let mut command = Command::new("cargo");
    command.current_dir(workspace_root()).args(args);
    let output = command.output().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => CargoError::Spawn {
            command: command_name.clone(),
            error: "cargo is not installed".to_owned(),
        },
        _ => CargoError::Spawn {
            command: command_name.clone(),
            error: error.to_string(),
        },
    })?;
    let output_text = combine_output(&output.stdout, &output.stderr);
    if !output.status.success() {
        return Err(CargoError::Failed {
            command: command_name,
            status: output.status.code(),
            output: output_text,
        });
    }
    Ok(output_text)
}

pub fn kernel_binary_path(workspace: &Path) -> PathBuf {
    workspace
        .join("target")
        .join(RISCV_TARGET)
        .join("debug")
        .join(KERNEL_BINARY)
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be in the workspace root")
        .to_owned()
}

fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut output = String::from_utf8_lossy(stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(stderr));
    output
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::{self, Command},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn failed_operation_preserves_command_status_and_diagnostics() {
        let error =
            run(&["minios-invalid-operation"]).expect_err("unknown Cargo operation must fail");

        match error {
            CargoError::Failed {
                command,
                status,
                output,
            } => {
                assert_eq!(command, "cargo minios-invalid-operation");
                assert!(status.is_some_and(|status| status != 0));
                assert!(output.contains("no such command"));
            }
            other => panic!("expected failed Cargo command, got {other:?}"),
        }
    }

    #[test]
    fn failed_operation_preserves_stdout_and_stderr() {
        let output = combine_output(b"stdout diagnostic\n", b"stderr diagnostic\n");
        let error = CargoError::Failed {
            command: "cargo fixture".to_owned(),
            status: Some(1),
            output,
        };

        let display = error.to_string();
        assert!(display.contains("stdout diagnostic"));
        assert!(display.contains("stderr diagnostic"));
    }

    #[test]
    fn linker_places_small_data_and_bss_probes_inside_boundaries() {
        let fixture = std::env::temp_dir().join(format!(
            "minios-linker-test-{}-{}",
            process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos()
        ));
        fs::create_dir(&fixture).expect("must create linker test directory");
        let source = fixture.join("small_bss.rs");
        let elf = fixture.join("small_bss.elf");
        fs::write(
            &source,
            r#"#![no_std]
#![no_main]

#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".sbss.linker_probe")]
static MINIOS_LINKER_SMALL_BSS_PROBE: u64 = 0;

#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".sdata.linker_probe")]
static MINIOS_LINKER_SMALL_DATA_PROBE: u64 = 1;

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
"#,
        )
        .expect("must write linker fixture");

        let linker = workspace_root().join("kernel/linker.ld");
        let output = Command::new("rustc")
            .args([
                source.as_os_str(),
                "--target".as_ref(),
                RISCV_TARGET.as_ref(),
                "-C".as_ref(),
                "panic=abort".as_ref(),
                "-C".as_ref(),
                format!("link-arg=-T{}", linker.display()).as_ref(),
                "-o".as_ref(),
                elf.as_os_str(),
            ])
            .output()
            .expect("rustc must start for linker fixture");
        assert!(
            output.status.success(),
            "linker fixture compilation failed:\n{}",
            combine_output(&output.stdout, &output.stderr)
        );

        let elf = fs::read(&elf).expect("must read fixture ELF");
        let bss_section = elf_symbol_section(&elf, "MINIOS_LINKER_SMALL_BSS_PROBE");
        let data_section = elf_symbol_section(&elf, "MINIOS_LINKER_SMALL_DATA_PROBE");
        let _ = fs::remove_dir_all(&fixture);
        assert_eq!(bss_section.as_deref(), Some(".bss"));
        assert_eq!(data_section.as_deref(), Some(".data"));
    }

    fn elf_symbol_section(elf: &[u8], symbol: &str) -> Option<String> {
        let section_headers = elf_section_headers(elf)?;
        let symbol_table = section_headers.iter().find(|header| header.kind == 2)?;
        let string_table = section_headers.get(symbol_table.link as usize)?;
        let names = elf.get(string_table.offset..string_table.offset + string_table.size)?;

        for offset in (symbol_table.offset..symbol_table.offset + symbol_table.size)
            .step_by(symbol_table.entry_size)
        {
            let name_offset = read_u32(elf, offset)? as usize;
            let section_index = read_u16(elf, offset + 6)? as usize;
            if elf_string(names, name_offset)? == symbol {
                return section_headers
                    .get(section_index)
                    .and_then(|header| {
                        elf_string(elf_section_name_table(elf, &section_headers)?, header.name)
                    })
                    .map(str::to_owned);
            }
        }
        None
    }

    #[derive(Clone, Copy)]
    struct ElfSectionHeader {
        name: usize,
        kind: u32,
        offset: usize,
        size: usize,
        link: u32,
        entry_size: usize,
    }

    fn elf_section_headers(elf: &[u8]) -> Option<Vec<ElfSectionHeader>> {
        if elf.get(0..4)? != b"\x7fELF" || *elf.get(4)? != 2 || *elf.get(5)? != 1 {
            return None;
        }
        let table_offset = read_u64(elf, 40)? as usize;
        let entry_size = read_u16(elf, 58)? as usize;
        let count = read_u16(elf, 60)? as usize;
        (0..count)
            .map(|index| {
                let offset = table_offset.checked_add(index.checked_mul(entry_size)?)?;
                Some(ElfSectionHeader {
                    name: read_u32(elf, offset)? as usize,
                    kind: read_u32(elf, offset + 4)?,
                    offset: read_u64(elf, offset + 24)? as usize,
                    size: read_u64(elf, offset + 32)? as usize,
                    link: read_u32(elf, offset + 40)?,
                    entry_size: read_u64(elf, offset + 56)? as usize,
                })
            })
            .collect()
    }

    fn elf_section_name_table<'a>(
        elf: &'a [u8],
        sections: &[ElfSectionHeader],
    ) -> Option<&'a [u8]> {
        let index = read_u16(elf, 62)? as usize;
        let section = sections.get(index)?;
        elf.get(section.offset..section.offset + section.size)
    }

    fn elf_string(table: &[u8], offset: usize) -> Option<&str> {
        let bytes = table.get(offset..)?;
        let end = bytes.iter().position(|byte| *byte == 0)?;
        std::str::from_utf8(&bytes[..end]).ok()
    }

    fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            bytes.get(offset..offset + 2)?.try_into().ok()?,
        ))
    }

    fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            bytes.get(offset..offset + 8)?.try_into().ok()?,
        ))
    }
}
