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
    Spawn(String),
    Failed { status: Option<i32>, output: String },
}

impl fmt::Display for CargoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "could not start cargo build: {error}"),
            Self::Failed { status, output } => write!(
                formatter,
                "cargo build failed with status {}:\n{}",
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
    let workspace = workspace_root();
    let mut command = Command::new("cargo");
    command.current_dir(&workspace).args([
        "build",
        "-p",
        KERNEL_PACKAGE,
        "--bin",
        KERNEL_BINARY,
        "--target",
        RISCV_TARGET,
    ]);
    if qemu_test_boot {
        command.args(["--features", "qemu-test-boot"]);
    }

    let output = command.output().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => CargoError::Spawn("cargo is not installed".to_owned()),
        _ => CargoError::Spawn(error.to_string()),
    })?;
    if !output.status.success() {
        return Err(CargoError::Failed {
            status: output.status.code(),
            output: combine_output(&output.stdout, &output.stderr),
        });
    }

    Ok(kernel_binary_path(&workspace))
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
