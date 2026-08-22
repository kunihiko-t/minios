use std::{
    fmt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::cargo;

const BOOT_MARKER: &str = "[MINIOS_TEST] boot: ok";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    Boot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QemuError {
    Build(cargo::CargoError),
    Spawn(String),
    Wait(String),
    Failed { status: Option<i32>, output: String },
    TimedOut(String),
    MissingMarker(String),
}

impl fmt::Display for QemuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => error.fmt(formatter),
            Self::Spawn(error) => write!(formatter, "could not start QEMU: {error}"),
            Self::Wait(error) => write!(formatter, "could not wait for QEMU: {error}"),
            Self::Failed { status, output } => write!(
                formatter,
                "QEMU exited with status {}:\n{}",
                status
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                output.trim_end()
            ),
            Self::TimedOut(output) => write!(
                formatter,
                "QEMU boot test timed out after five seconds:\n{}",
                output.trim_end()
            ),
            Self::MissingMarker(output) => write!(
                formatter,
                "QEMU exited successfully but did not print {BOOT_MARKER}:\n{}",
                output.trim_end()
            ),
        }
    }
}

impl std::error::Error for QemuError {}

pub fn run_kernel() -> Result<(), QemuError> {
    let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
    let status = Command::new("qemu-system-riscv64")
        .args(qemu_args(&kernel))
        .status()
        .map_err(|error| QemuError::Spawn(error.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(QemuError::Failed {
            status: status.code(),
            output: String::new(),
        })
    }
}

pub fn run_test(kind: TestKind, deadline: Duration) -> Result<String, QemuError> {
    let kernel = match kind {
        TestKind::Boot => cargo::build_kernel(true).map_err(QemuError::Build)?,
    };
    let mut child = Command::new("qemu-system-riscv64")
        .args(qemu_args(&kernel))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| QemuError::Spawn(error.to_string()))?;
    let started = Instant::now();

    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| QemuError::Wait(error.to_string()))?
        {
            let output = child
                .wait_with_output()
                .map_err(|error| QemuError::Wait(error.to_string()))?;
            return verify_boot_result(
                status.code(),
                &combine_output(&output.stdout, &output.stderr),
            );
        }
        if started.elapsed() >= deadline {
            child
                .kill()
                .map_err(|error| QemuError::Wait(error.to_string()))?;
            let output = child
                .wait_with_output()
                .map_err(|error| QemuError::Wait(error.to_string()))?;
            return Err(QemuError::TimedOut(combine_output(
                &output.stdout,
                &output.stderr,
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn qemu_args(kernel: &Path) -> Vec<String> {
    vec![
        "-machine".to_owned(),
        "virt".to_owned(),
        "-m".to_owned(),
        "128M".to_owned(),
        "-smp".to_owned(),
        "1".to_owned(),
        "-bios".to_owned(),
        "default".to_owned(),
        "-kernel".to_owned(),
        kernel.display().to_string(),
        "-serial".to_owned(),
        "stdio".to_owned(),
        "-monitor".to_owned(),
        "none".to_owned(),
        "-display".to_owned(),
        "none".to_owned(),
    ]
}

#[cfg(test)]
fn contains_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

fn verify_boot_result(status: Option<i32>, output: &str) -> Result<String, QemuError> {
    if status != Some(0) {
        return Err(QemuError::Failed {
            status,
            output: output.to_owned(),
        });
    }
    if !output.contains(BOOT_MARKER) {
        return Err(QemuError::MissingMarker(output.to_owned()));
    }
    Ok(output.to_owned())
}

fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut output = String::from_utf8_lossy(stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(stderr));
    output
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn test_qemu_args_are_headless_and_single_hart() {
        let args = qemu_args(Path::new("kernel.elf"));
        assert!(contains_pair(&args, "-machine", "virt"));
        assert!(contains_pair(&args, "-m", "128M"));
        assert!(contains_pair(&args, "-smp", "1"));
        assert!(contains_pair(&args, "-bios", "default"));
        assert!(contains_pair(&args, "-kernel", "kernel.elf"));
        assert!(contains_pair(&args, "-serial", "stdio"));
        assert!(contains_pair(&args, "-monitor", "none"));
        assert!(contains_pair(&args, "-display", "none"));
    }

    #[test]
    fn successful_boot_requires_the_exact_marker() {
        let output = "MiniOS booting...\n";

        assert_eq!(
            verify_boot_result(Some(0), output),
            Err(QemuError::MissingMarker(output.to_owned()))
        );
    }
}
