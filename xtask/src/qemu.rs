use std::{
    fmt, io,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::cargo;

const BOOT_MARKER: &str = "[MINIOS_TEST] boot: ok";
const TRAP_MARKER: &str = "[MINIOS_TEST] trap: ok";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    Boot,
    Trap,
}

impl TestKind {
    fn feature(self) -> &'static str {
        match self {
            Self::Boot => "qemu-test-boot",
            Self::Trap => "qemu-test-trap",
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::Boot => BOOT_MARKER,
            Self::Trap => TRAP_MARKER,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QemuError {
    Build(cargo::CargoError),
    Spawn(String),
    Wait(String),
    Failed {
        status: Option<i32>,
        output: String,
    },
    TimedOut(String),
    MissingMarker {
        expected: &'static str,
        output: String,
    },
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
                "QEMU test timed out after five seconds:\n{}",
                output.trim_end()
            ),
            Self::MissingMarker { expected, output } => write!(
                formatter,
                "QEMU exited successfully but did not print {expected}:\n{}",
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
    let kernel = cargo::build_kernel_for_test(kind.feature()).map_err(QemuError::Build)?;
    let mut command = Command::new("qemu-system-riscv64");
    command.args(qemu_args(&kernel));
    let completed = run_command_with_capture(command, deadline)?;
    verify_test_result(kind, completed.status.code(), &completed.output)
}

#[derive(Debug)]
struct CompletedProcess {
    status: ExitStatus,
    output: String,
}

fn run_command_with_capture(
    mut command: Command,
    deadline: Duration,
) -> Result<CompletedProcess, QemuError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| QemuError::Spawn(error.to_string()))?;
    let readers = OutputReaders::start(&mut child);

    match wait_until_exit(&mut child, deadline) {
        Ok(status) => {
            let output = readers.join().map_err(QemuError::Wait)?;
            Ok(CompletedProcess { status, output })
        }
        Err(WaitFailure::TimedOut) => {
            let cleanup = terminate_and_reap(&mut child);
            let mut output = readers.join().unwrap_or_else(|error| error);
            if let Err(error) = cleanup {
                output.push_str("\nQEMU cleanup error: ");
                output.push_str(&error);
            }
            Err(QemuError::TimedOut(output))
        }
        Err(WaitFailure::Poll(error)) => {
            let cleanup = terminate_and_reap(&mut child);
            let output = readers.join().unwrap_or_else(|error| error);
            let cleanup = cleanup
                .err()
                .map(|error| format!("; cleanup also failed: {error}"))
                .unwrap_or_default();
            Err(QemuError::Wait(format!("{error}{cleanup}\n{output}")))
        }
    }
}

enum WaitFailure {
    TimedOut,
    Poll(String),
}

fn wait_until_exit(child: &mut Child, deadline: Duration) -> Result<ExitStatus, WaitFailure> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() >= deadline => return Err(WaitFailure::TimedOut),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(WaitFailure::Poll(error.to_string())),
        }
    }
}

fn terminate_and_reap(child: &mut Child) -> Result<(), String> {
    let kill_error = child.kill().err().map(|error| error.to_string());
    match child.wait() {
        Ok(_) => Ok(()),
        Err(wait_error) => {
            let _ = child.kill();
            let retry_wait = child.wait().err().map(|error| error.to_string());
            Err(format!(
                "kill error: {}; wait error: {}; retry wait error: {}",
                kill_error.unwrap_or_else(|| "none".to_owned()),
                wait_error,
                retry_wait.unwrap_or_else(|| "none".to_owned())
            ))
        }
    }
}

struct OutputReaders {
    stdout: thread::JoinHandle<io::Result<Vec<u8>>>,
    stderr: thread::JoinHandle<io::Result<Vec<u8>>>,
}

impl OutputReaders {
    fn start(child: &mut Child) -> Self {
        let stdout = child.stdout.take().expect("stdout must be piped");
        let stderr = child.stderr.take().expect("stderr must be piped");
        Self {
            stdout: thread::spawn(move || read_stream(stdout)),
            stderr: thread::spawn(move || read_stream(stderr)),
        }
    }

    fn join(self) -> Result<String, String> {
        let stdout = join_reader(self.stdout)?;
        let stderr = join_reader(self.stderr)?;
        Ok(combine_output(&stdout, &stderr))
    }
}

fn read_stream(mut stream: impl io::Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| "output reader thread panicked".to_owned())?
        .map_err(|error| error.to_string())
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

fn verify_test_result(
    kind: TestKind,
    status: Option<i32>,
    output: &str,
) -> Result<String, QemuError> {
    if status != Some(0) {
        return Err(QemuError::Failed {
            status,
            output: output.to_owned(),
        });
    }
    if !output.contains(kind.marker()) {
        return Err(QemuError::MissingMarker {
            expected: kind.marker(),
            output: output.to_owned(),
        });
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
    use std::{
        fs,
        path::Path,
        process::{self, Command},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

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
            verify_test_result(TestKind::Boot, Some(0), output),
            Err(QemuError::MissingMarker {
                expected: BOOT_MARKER,
                output: output.to_owned(),
            })
        );
    }

    #[test]
    fn successful_trap_requires_the_exact_trap_marker() {
        let output = "[MINIOS_TEST] boot: ok\n";

        assert_eq!(
            verify_test_result(TestKind::Trap, Some(0), output),
            Err(QemuError::MissingMarker {
                expected: TRAP_MARKER,
                output: output.to_owned(),
            })
        );
    }

    #[test]
    fn drains_large_stdout_and_stderr_before_the_deadline() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "yes stdout | head -c 131072; yes stderr | head -c 131072 >&2; printf '[MINIOS_TEST] boot: ok'",
        ]);

        let completed = run_command_with_capture(command, Duration::from_secs(2))
            .expect("concurrently drained process must complete");

        assert_eq!(completed.status.code(), Some(0));
        assert!(completed.output.len() >= 262_144);
        assert!(completed.output.contains(BOOT_MARKER));
    }

    #[test]
    fn timeout_reaps_process_and_preserves_both_streams() {
        let pid_file = std::env::temp_dir().join(format!(
            "minios-qemu-test-pid-{}-{}",
            process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos()
        ));
        let mut command = Command::new("sh");
        command.args([
            "-c",
            &format!(
                "echo $$ > '{}'; printf stdout-before-timeout; printf stderr-before-timeout >&2; exec sleep 30",
                shell_quote_path(&pid_file)
            ),
        ]);

        let error = run_command_with_capture(command, Duration::from_millis(50))
            .expect_err("sleeping process must time out");
        let output = match error {
            QemuError::TimedOut(output) => output,
            other => panic!("expected timeout, got {other:?}"),
        };

        assert!(output.contains("stdout-before-timeout"));
        assert!(output.contains("stderr-before-timeout"));
        let pid = fs::read_to_string(&pid_file).expect("timed-out process must record its PID");
        let status = Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("kill must start");
        let _ = fs::remove_file(&pid_file);
        assert!(!status.success(), "timed-out child must have been reaped");
    }

    fn shell_quote_path(path: &Path) -> String {
        path.display().to_string().replace('\'', "'\\''")
    }
}
