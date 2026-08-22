use std::{fmt, io, process::Command};

const RISCV_TARGET: &str = "riscv64gc-unknown-none-elf";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    CommandUnavailable(&'static str),
    CommandFailed {
        command: &'static str,
        status: Option<i32>,
        stderr: String,
    },
    MissingRustTarget,
    MalformedQemuVersion,
    UnsupportedQemuVersion(Version),
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommandUnavailable("rustc") => write!(
                formatter,
                "rustc is not installed. Install it with: rustup toolchain install 1.98.0"
            ),
            Self::CommandUnavailable("rustup") => write!(
                formatter,
                "rustup is not installed. Install it with: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
            ),
            Self::CommandUnavailable("qemu-system-riscv64") => write!(
                formatter,
                "qemu-system-riscv64 is not installed. Install it with: brew install qemu"
            ),
            Self::CommandUnavailable(command) => {
                write!(formatter, "{command} is not installed")
            }
            Self::CommandFailed {
                command,
                status,
                stderr,
            } => write!(
                formatter,
                "{command} failed with status {}: {}",
                status
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                stderr.trim()
            ),
            Self::MissingRustTarget => write!(
                formatter,
                "Rust target {RISCV_TARGET} is not installed. Install it with: rustup target add {RISCV_TARGET}"
            ),
            Self::MalformedQemuVersion => {
                write!(formatter, "could not parse QEMU emulator version X.Y.Z")
            }
            Self::UnsupportedQemuVersion(version) => write!(
                formatter,
                "QEMU {}.{}.{} is too old; QEMU 9.0.0 or newer is required",
                version.major, version.minor, version.patch
            ),
        }
    }
}

impl std::error::Error for ToolError {}

pub fn parse_qemu_version(output: &str) -> Result<Version, ToolError> {
    let line = output
        .lines()
        .next()
        .ok_or(ToolError::MalformedQemuVersion)?;
    let version = line
        .strip_prefix("QEMU emulator version ")
        .ok_or(ToolError::MalformedQemuVersion)?;
    let mut components = version.split('.');
    let major = parse_component(components.next())?;
    let minor = parse_component(components.next())?;
    let patch = parse_component(components.next())?;

    if components.next().is_some() {
        return Err(ToolError::MalformedQemuVersion);
    }

    let version = Version {
        major,
        minor,
        patch,
    };
    if version < minimum_qemu_version() {
        return Err(ToolError::UnsupportedQemuVersion(version));
    }

    Ok(version)
}

pub fn check_setup() -> Result<(), ToolError> {
    let rustc = run_command("rustc", &["--version"])?;
    println!("Rust: {}", first_line(&rustc)?);

    let targets = run_command("rustup", &["target", "list", "--installed"])?;
    if !targets.lines().any(|target| target == RISCV_TARGET) {
        return Err(ToolError::MissingRustTarget);
    }
    println!("Rust target: {RISCV_TARGET}");

    let qemu = run_command("qemu-system-riscv64", &["--version"])?;
    let version = parse_qemu_version(&qemu)?;
    println!(
        "QEMU: {}.{}.{}",
        version.major, version.minor, version.patch
    );

    Ok(())
}

fn minimum_qemu_version() -> Version {
    Version {
        major: 9,
        minor: 0,
        patch: 0,
    }
}

fn parse_component(component: Option<&str>) -> Result<u32, ToolError> {
    component
        .filter(|value| !value.is_empty())
        .ok_or(ToolError::MalformedQemuVersion)?
        .parse()
        .map_err(|_| ToolError::MalformedQemuVersion)
}

fn run_command(command: &'static str, args: &[&str]) -> Result<String, ToolError> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => ToolError::CommandUnavailable(command),
            _ => ToolError::CommandFailed {
                command,
                status: None,
                stderr: error.to_string(),
            },
        })?;

    if !output.status.success() {
        return Err(ToolError::CommandFailed {
            command,
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn first_line(output: &str) -> Result<&str, ToolError> {
    output.lines().next().ok_or(ToolError::MalformedQemuVersion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_version_from_the_first_qemu_output_line() {
        let output = "QEMU emulator version 9.2.3\nQEMU is free software";

        assert_eq!(
            parse_qemu_version(output),
            Ok(Version {
                major: 9,
                minor: 2,
                patch: 3,
            })
        );
    }

    #[test]
    fn rejects_malformed_qemu_version_output() {
        assert_eq!(
            parse_qemu_version("QEMU emulator version 9.2\n"),
            Err(ToolError::MalformedQemuVersion)
        );
    }

    #[test]
    fn rejects_qemu_versions_older_than_nine() {
        assert_eq!(
            parse_qemu_version("QEMU emulator version 8.2.0\n"),
            Err(ToolError::UnsupportedQemuVersion(Version {
                major: 8,
                minor: 2,
                patch: 0,
            }))
        );
    }
}
