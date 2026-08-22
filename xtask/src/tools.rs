use std::{fmt, io, process::Command};

const RISCV_TARGET: &str = "riscv64gc-unknown-none-elf";

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPlatform {
    Linux,
    Macos,
    Other,
}

impl HostPlatform {
    fn current() -> Self {
        #[cfg(target_os = "linux")]
        return Self::Linux;

        #[cfg(target_os = "macos")]
        return Self::Macos;

        #[allow(unreachable_code)]
        Self::Other
    }
}

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
    MalformedRustcVersion,
    UnsupportedRustcVersion(String),
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
            Self::CommandUnavailable("qemu-system-riscv64") => {
                write!(
                    formatter,
                    "{}",
                    missing_qemu_message(HostPlatform::current())
                )
            }
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
            Self::MalformedRustcVersion => write!(
                formatter,
                "could not parse rustc version; expected rustc X.Y.Z with an optional build description"
            ),
            Self::UnsupportedRustcVersion(version) => write!(
                formatter,
                "Rust {version} is not supported; exact Rust 1.98.0 stable is required"
            ),
            Self::MalformedQemuVersion => {
                write!(formatter, "could not parse QEMU emulator version X.Y.Z")
            }
            Self::UnsupportedQemuVersion(version) => write!(
                formatter,
                "QEMU {}.{}.{} is too old; QEMU 8.2.0 or newer is required",
                version.major, version.minor, version.patch
            ),
        }
    }
}

impl std::error::Error for ToolError {}

fn missing_qemu_message(platform: HostPlatform) -> &'static str {
    match platform {
        HostPlatform::Macos => {
            "qemu-system-riscv64 is not installed. Install it with: brew install qemu"
        }
        HostPlatform::Linux => {
            "qemu-system-riscv64 is not installed. Install it with: sudo apt install qemu-system-misc"
        }
        HostPlatform::Other => {
            "qemu-system-riscv64 is not installed. Install the QEMU package that provides qemu-system-riscv64."
        }
    }
}

pub fn parse_qemu_version(output: &str) -> Result<Version, ToolError> {
    let line = output
        .lines()
        .next()
        .ok_or(ToolError::MalformedQemuVersion)?;
    let version = line
        .strip_prefix("QEMU emulator version ")
        .ok_or(ToolError::MalformedQemuVersion)?;
    let version_token = version
        .split_whitespace()
        .next()
        .ok_or(ToolError::MalformedQemuVersion)?;
    let mut components = version_token.split('.');
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

pub fn parse_rustc_version(output: &str) -> Result<Version, ToolError> {
    let line = output
        .lines()
        .next()
        .ok_or(ToolError::MalformedRustcVersion)?;
    let mut fields = line.split_whitespace();
    if fields.next() != Some("rustc") {
        return Err(ToolError::MalformedRustcVersion);
    }
    let token = fields.next().ok_or(ToolError::MalformedRustcVersion)?;
    let (numeric, suffix) = match token.split_once('-') {
        Some((_numeric, "")) => {
            return Err(ToolError::MalformedRustcVersion);
        }
        Some((numeric, suffix))
            if suffix
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '.') =>
        {
            (numeric, Some(suffix))
        }
        Some(_) => return Err(ToolError::MalformedRustcVersion),
        None => (token, None),
    };
    let version = parse_numeric_version(numeric).ok_or(ToolError::MalformedRustcVersion)?;
    if suffix.is_some() || version != pinned_rust_version() {
        return Err(ToolError::UnsupportedRustcVersion(token.to_owned()));
    }
    Ok(version)
}

pub fn check_setup() -> Result<(), ToolError> {
    let rustc = run_command("rustc", &["--version"])?;
    parse_rustc_version(&rustc)?;
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
        major: 8,
        minor: 2,
        patch: 0,
    }
}

fn pinned_rust_version() -> Version {
    Version {
        major: 1,
        minor: 98,
        patch: 0,
    }
}

fn parse_numeric_version(token: &str) -> Option<Version> {
    let mut components = token.split('.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    let patch = components.next()?.parse().ok()?;
    if components.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
    })
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
    output
        .lines()
        .next()
        .ok_or(ToolError::MalformedRustcVersion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_a_homebrew_command_for_missing_qemu_on_macos() {
        assert_eq!(
            missing_qemu_message(HostPlatform::Macos),
            "qemu-system-riscv64 is not installed. Install it with: brew install qemu"
        );
    }

    #[test]
    fn gives_an_apt_command_for_missing_qemu_on_linux() {
        assert_eq!(
            missing_qemu_message(HostPlatform::Linux),
            "qemu-system-riscv64 is not installed. Install it with: sudo apt install qemu-system-misc"
        );
    }

    #[test]
    fn accepts_the_exact_pinned_stable_rustc_version() {
        let output = "rustc 1.98.0 (88d9e12ae 2026-08-18)\n";

        assert_eq!(
            parse_rustc_version(output),
            Ok(Version {
                major: 1,
                minor: 98,
                patch: 0,
            })
        );
    }

    #[test]
    fn rejects_a_different_stable_rustc_version_precisely() {
        let error = parse_rustc_version("rustc 1.98.1 (future)\n")
            .expect_err("a non-pinned stable compiler must be rejected");

        assert_eq!(
            error,
            ToolError::UnsupportedRustcVersion("1.98.1".to_owned())
        );
        assert_eq!(
            error.to_string(),
            "Rust 1.98.1 is not supported; exact Rust 1.98.0 stable is required"
        );
    }

    #[test]
    fn rejects_a_rustc_channel_suffix_precisely() {
        let error = parse_rustc_version("rustc 1.98.0-nightly (nightly)\n")
            .expect_err("a suffixed compiler is not the pinned stable compiler");

        assert_eq!(
            error,
            ToolError::UnsupportedRustcVersion("1.98.0-nightly".to_owned())
        );
        assert_eq!(
            error.to_string(),
            "Rust 1.98.0-nightly is not supported; exact Rust 1.98.0 stable is required"
        );
    }

    #[test]
    fn rejects_malformed_rustc_version_output_without_panicking() {
        for output in [
            "",
            "cargo 1.98.0\n",
            "rustc\n",
            "rustc 1.98\n",
            "rustc 1.x.0\n",
            "rustc 1.98.0.1\n",
            "rustc 1.98.0-\n",
        ] {
            assert_eq!(
                parse_rustc_version(output),
                Err(ToolError::MalformedRustcVersion),
                "unexpectedly accepted {output:?}"
            );
        }
    }

    #[test]
    fn parses_the_local_version_from_the_first_qemu_output_line() {
        let output = "QEMU emulator version 11.1.0\nQEMU is free software";

        assert_eq!(
            parse_qemu_version(output),
            Ok(Version {
                major: 11,
                minor: 1,
                patch: 0,
            })
        );
    }

    #[test]
    fn parses_a_packaged_qemu_version_with_a_suffix() {
        let output = "QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.6)\nCopyright";

        assert_eq!(
            parse_qemu_version(output),
            Ok(Version {
                major: 8,
                minor: 2,
                patch: 2,
            })
        );
    }

    #[test]
    fn rejects_malformed_qemu_version_output() {
        for output in [
            "QEMU emulator version 9.2\n",
            "QEMU emulator version 9.x.3\n",
            "",
            "QEMU version 9.2.3\n",
        ] {
            assert_eq!(
                parse_qemu_version(output),
                Err(ToolError::MalformedQemuVersion),
                "unexpectedly accepted {output:?}"
            );
        }
    }

    #[test]
    fn accepts_qemu_8_2_0_as_the_compatibility_floor() {
        assert_eq!(
            parse_qemu_version("QEMU emulator version 8.2.0\n"),
            Ok(Version {
                major: 8,
                minor: 2,
                patch: 0,
            })
        );
    }

    #[test]
    fn rejects_qemu_8_1_x_with_the_compatibility_floor() {
        let error = parse_qemu_version("QEMU emulator version 8.1.9\n")
            .expect_err("QEMU before 8.2.0 must be rejected");

        assert_eq!(
            error,
            ToolError::UnsupportedQemuVersion(Version {
                major: 8,
                minor: 1,
                patch: 9,
            })
        );
        assert_eq!(
            error.to_string(),
            "QEMU 8.1.9 is too old; QEMU 8.2.0 or newer is required"
        );
    }
}
