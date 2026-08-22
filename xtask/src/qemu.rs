use std::{
    fmt,
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use crate::cargo;

const QEMU_PROGRAM: &str = "qemu-system-riscv64";
const BOOT_MARKER: &str = "[MINIOS_TEST] boot: ok";
const TIMER_MARKER: &str = "[MINIOS_TEST] timer: ok";
const TRAP_MARKER: &str = "[MINIOS_TEST] trap: ok";
const MEMORY_MARKER: &str = "[MINIOS_TEST] memory: ok";
const SHELL_PROMPT: &str = "minios> ";
const SHELL_SCRIPT: &[u8] = b"help\ninfo\nuptime\nmemory\nnot-a-command\nshutdown\n";
const SHELL_UPTIME_FORMAT: &str = "uptime: <number> ms";
const SHELL_TICKS_FORMAT: &str = "ticks: <number>";
const SHELL_MEMORY_FORMAT: &str = "memory: total=<number> allocated=<number> free=<number> pages";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    Boot,
    Timer,
    Trap,
    Memory,
    Shell,
}

impl TestKind {
    fn feature(self) -> &'static str {
        match self {
            Self::Boot => "qemu-test-boot",
            Self::Timer => "qemu-test-timer",
            Self::Trap => "qemu-test-trap",
            Self::Memory => "qemu-test-memory",
            Self::Shell => unreachable!("the shell test boots the normal kernel"),
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::Boot => BOOT_MARKER,
            Self::Timer => TIMER_MARKER,
            Self::Trap => TRAP_MARKER,
            Self::Memory => MEMORY_MARKER,
            Self::Shell => unreachable!("the shell test verifies an interactive transcript"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QemuError {
    Build(cargo::CargoError),
    Spawn {
        command: String,
        error: String,
    },
    Wait {
        command: String,
        error: String,
    },
    Failed {
        command: String,
        status: Option<i32>,
        output: String,
    },
    TimedOut {
        command: String,
        deadline: Duration,
        output: String,
    },
    MissingMarker {
        command: String,
        expected: &'static str,
        output: String,
    },
    MissingShellOutput {
        command: String,
        expected: &'static str,
        output: String,
    },
}

impl fmt::Display for QemuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => error.fmt(formatter),
            Self::Spawn { command, error } => write!(
                formatter,
                "could not start QEMU:\ncommand: {command}\n{error}"
            ),
            Self::Wait { command, error } => write!(
                formatter,
                "could not wait for QEMU:\ncommand: {command}\n{error}"
            ),
            Self::Failed {
                command,
                status,
                output,
            } => write!(
                formatter,
                "QEMU exited with status {}:\ncommand: {command}\n{}",
                status
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                output.trim_end()
            ),
            Self::TimedOut {
                command,
                deadline,
                output,
            } => write!(
                formatter,
                "QEMU test timed out after {:.3} seconds:\ncommand: {command}\n{}",
                deadline.as_secs_f64(),
                output.trim_end()
            ),
            Self::MissingMarker {
                command,
                expected,
                output,
            } => write!(
                formatter,
                "QEMU exited successfully but did not print {expected}:\ncommand: {command}\n{}",
                output.trim_end()
            ),
            Self::MissingShellOutput {
                command,
                expected,
                output,
            } => write!(
                formatter,
                "QEMU shell transcript did not match expected line/position {expected:?}:\ncommand: {command}\n{}",
                output.trim_end()
            ),
        }
    }
}

impl std::error::Error for QemuError {}

pub fn run_kernel() -> Result<(), QemuError> {
    let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
    let (mut command, command_line) = qemu_command(&kernel);
    let status = command.status().map_err(|error| QemuError::Spawn {
        command: command_line.clone(),
        error: error.to_string(),
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(QemuError::Failed {
            command: command_line,
            status: status.code(),
            output: String::new(),
        })
    }
}

pub fn run_test(kind: TestKind, deadline: Duration) -> Result<String, QemuError> {
    if kind == TestKind::Shell {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let (command, command_line) = qemu_command(&kernel);
        let completed = run_shell_command(command, command_line.clone(), deadline)?;
        return verify_shell_result(&command_line, completed.status.code(), &completed.output);
    }

    let kernel = cargo::build_kernel_for_test(kind.feature()).map_err(QemuError::Build)?;
    let (command, command_line) = qemu_command(&kernel);
    let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
    verify_test_result(
        &command_line,
        kind,
        completed.status.code(),
        &completed.output,
    )
}

fn run_shell_command(
    mut command: Command,
    command_line: String,
    deadline: Duration,
) -> Result<CompletedProcess, QemuError> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| QemuError::Spawn {
            command: command_line.clone(),
            error: error.to_string(),
        })?;
    let readers = LiveOutputReaders::start(&mut child);
    let started = Instant::now();

    if let Err(failure) = wait_for_output(&mut child, &readers, SHELL_PROMPT, started, deadline) {
        return finish_shell_failure(child, readers, command_line, deadline, failure);
    }

    let write_result = child
        .stdin
        .take()
        .expect("shell test stdin must be piped")
        .write_all(SHELL_SCRIPT);
    if let Err(error) = write_result {
        let cleanup = terminate_and_reap(&mut child);
        let mut output = readers.join().unwrap_or_else(|join_error| join_error);
        if let Err(cleanup_error) = cleanup {
            output.push_str("\nQEMU cleanup error: ");
            output.push_str(&cleanup_error);
        }
        return Err(QemuError::Wait {
            command: command_line,
            error: format!("could not write shell script: {error}\n{output}"),
        });
    }

    let remaining = deadline.saturating_sub(started.elapsed());
    match wait_until_exit(&mut child, remaining) {
        Ok(status) => {
            let output = readers.join().map_err(|error| QemuError::Wait {
                command: command_line.clone(),
                error,
            })?;
            Ok(CompletedProcess { status, output })
        }
        Err(failure) => {
            finish_shell_failure(child, readers, command_line, deadline, failure.into())
        }
    }
}

fn wait_for_output(
    child: &mut Child,
    readers: &LiveOutputReaders,
    expected: &str,
    started: Instant,
    deadline: Duration,
) -> Result<(), ShellFailure> {
    loop {
        if readers.contains(expected) {
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(_)) => return Err(ShellFailure::Exited),
            Ok(None) if started.elapsed() >= deadline => return Err(ShellFailure::TimedOut),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(ShellFailure::Poll(error.to_string())),
        }
    }
}

fn finish_shell_failure(
    mut child: Child,
    readers: LiveOutputReaders,
    command: String,
    deadline: Duration,
    failure: ShellFailure,
) -> Result<CompletedProcess, QemuError> {
    let cleanup = terminate_and_reap(&mut child);
    let mut output = readers.join().unwrap_or_else(|error| error);
    if let Err(error) = cleanup {
        output.push_str("\nQEMU cleanup error: ");
        output.push_str(&error);
    }
    match failure {
        ShellFailure::TimedOut => Err(QemuError::TimedOut {
            command,
            deadline,
            output,
        }),
        ShellFailure::Poll(error) => Err(QemuError::Wait {
            command,
            error: format!("{error}\n{output}"),
        }),
        ShellFailure::Exited => Err(QemuError::MissingShellOutput {
            command,
            expected: SHELL_PROMPT,
            output,
        }),
    }
}

#[derive(Debug)]
struct CompletedProcess {
    status: ExitStatus,
    output: String,
}

fn run_command_with_capture(
    mut command: Command,
    command_line: String,
    deadline: Duration,
) -> Result<CompletedProcess, QemuError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| QemuError::Spawn {
            command: command_line.clone(),
            error: error.to_string(),
        })?;
    let readers = OutputReaders::start(&mut child);

    match wait_until_exit(&mut child, deadline) {
        Ok(status) => {
            let output = readers.join().map_err(|error| QemuError::Wait {
                command: command_line.clone(),
                error,
            })?;
            Ok(CompletedProcess { status, output })
        }
        Err(WaitFailure::TimedOut) => {
            let cleanup = terminate_and_reap(&mut child);
            let mut output = readers.join().unwrap_or_else(|error| error);
            if let Err(error) = cleanup {
                output.push_str("\nQEMU cleanup error: ");
                output.push_str(&error);
            }
            Err(QemuError::TimedOut {
                command: command_line,
                deadline,
                output,
            })
        }
        Err(WaitFailure::Poll(error)) => {
            let cleanup = terminate_and_reap(&mut child);
            let output = readers.join().unwrap_or_else(|error| error);
            let cleanup = cleanup
                .err()
                .map(|error| format!("; cleanup also failed: {error}"))
                .unwrap_or_default();
            Err(QemuError::Wait {
                command: command_line,
                error: format!("{error}{cleanup}\n{output}"),
            })
        }
    }
}

enum WaitFailure {
    TimedOut,
    Poll(String),
}

enum ShellFailure {
    TimedOut,
    Poll(String),
    Exited,
}

impl From<WaitFailure> for ShellFailure {
    fn from(failure: WaitFailure) -> Self {
        match failure {
            WaitFailure::TimedOut => Self::TimedOut,
            WaitFailure::Poll(error) => Self::Poll(error),
        }
    }
}

struct LiveOutputReaders {
    output: Arc<Mutex<Vec<u8>>>,
    stdout: thread::JoinHandle<io::Result<()>>,
    stderr: thread::JoinHandle<io::Result<()>>,
}

impl LiveOutputReaders {
    fn start(child: &mut Child) -> Self {
        let stdout = child.stdout.take().expect("stdout must be piped");
        let stderr = child.stderr.take().expect("stderr must be piped");
        let output = Arc::new(Mutex::new(Vec::new()));
        Self {
            stdout: spawn_live_reader(stdout, Arc::clone(&output)),
            stderr: spawn_live_reader(stderr, Arc::clone(&output)),
            output,
        }
    }

    fn contains(&self, expected: &str) -> bool {
        let output = self.output.lock().expect("live output mutex poisoned");
        output
            .windows(expected.len())
            .any(|window| window == expected.as_bytes())
    }

    fn join(self) -> Result<String, String> {
        join_live_reader(self.stdout)?;
        join_live_reader(self.stderr)?;
        let output = self.output.lock().map_err(|error| error.to_string())?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }
}

fn spawn_live_reader(
    mut stream: impl Read + Send + 'static,
    output: Arc<Mutex<Vec<u8>>>,
) -> thread::JoinHandle<io::Result<()>> {
    thread::spawn(move || {
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Ok(());
            }
            output
                .lock()
                .expect("live output mutex poisoned")
                .extend_from_slice(&chunk[..read]);
        }
    })
}

fn join_live_reader(reader: thread::JoinHandle<io::Result<()>>) -> Result<(), String> {
    reader
        .join()
        .map_err(|_| "live output reader thread panicked".to_owned())?
        .map_err(|error| error.to_string())
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

fn qemu_command(kernel: &Path) -> (Command, String) {
    let args = qemu_args(kernel);
    let command_line = render_command(QEMU_PROGRAM, &args);
    let mut command = Command::new(QEMU_PROGRAM);
    command.args(&args);
    (command, command_line)
}

#[cfg(test)]
fn qemu_command_line(kernel: &Path) -> String {
    render_command(QEMU_PROGRAM, &qemu_args(kernel))
}

fn render_command(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
    command: &str,
    kind: TestKind,
    status: Option<i32>,
    output: &str,
) -> Result<String, QemuError> {
    if status != Some(0) {
        return Err(QemuError::Failed {
            command: command.to_owned(),
            status,
            output: output.to_owned(),
        });
    }
    let normalized = output.replace("\r\n", "\n");
    if !normalized.lines().any(|line| line == kind.marker()) {
        return Err(QemuError::MissingMarker {
            command: command.to_owned(),
            expected: kind.marker(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_shell_result(
    command: &str,
    status: Option<i32>,
    output: &str,
) -> Result<String, QemuError> {
    if status != Some(0) {
        return Err(QemuError::Failed {
            command: command.to_owned(),
            status,
            output: output.to_owned(),
        });
    }
    let normalized = output.replace("\r\n", "\n");
    let lines: Vec<_> = normalized.lines().collect();
    let Some(first_prompt) = lines.iter().position(|line| line.starts_with(SHELL_PROMPT)) else {
        return Err(shell_transcript_error(command, "minios> help", output));
    };
    let transcript = &lines[first_prompt..];
    let mut cursor = 0;

    expect_shell_line(transcript, &mut cursor, "minios> help")
        .and_then(|()| {
            expect_shell_line(transcript, &mut cursor, "help      Show available commands")
        })
        .and_then(|()| {
            expect_shell_line(transcript, &mut cursor, "info      Show system information")
        })
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "uptime    Show elapsed time"))
        .and_then(|()| {
            expect_shell_line(
                transcript,
                &mut cursor,
                "memory    Show physical memory statistics",
            )
        })
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "clear     Clear the terminal"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "shutdown  Shut down MiniOS"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> info"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "MiniOS 0.1.0 on RISC-V 64"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "hart id: 0"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> uptime"))
        .and_then(|()| {
            expect_shell_dynamic_line(
                transcript,
                &mut cursor,
                SHELL_UPTIME_FORMAT,
                line_has_uptime,
            )
        })
        .and_then(|()| {
            expect_shell_dynamic_line(transcript, &mut cursor, SHELL_TICKS_FORMAT, line_has_ticks)
        })
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> memory"))
        .and_then(|()| {
            expect_shell_dynamic_line(
                transcript,
                &mut cursor,
                SHELL_MEMORY_FORMAT,
                line_has_memory_stats,
            )
        })
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> not-a-command"))
        .and_then(|()| {
            expect_shell_line(
                transcript,
                &mut cursor,
                "unknown command: not-a-command; try 'help'",
            )
        })
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> shutdown"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "shutting down"))
        .map_err(|expected| shell_transcript_error(command, expected, output))?;

    if transcript[cursor..].iter().any(|line| !line.is_empty()) {
        return Err(shell_transcript_error(
            command,
            "end of shell transcript",
            output,
        ));
    }
    Ok(output.to_owned())
}

fn shell_transcript_error(command: &str, expected: &'static str, output: &str) -> QemuError {
    QemuError::MissingShellOutput {
        command: command.to_owned(),
        expected,
        output: output.to_owned(),
    }
}

fn expect_shell_line(
    lines: &[&str],
    cursor: &mut usize,
    expected: &'static str,
) -> Result<(), &'static str> {
    if lines.get(*cursor).copied() != Some(expected) {
        return Err(expected);
    }
    *cursor += 1;
    Ok(())
}

fn expect_shell_dynamic_line(
    lines: &[&str],
    cursor: &mut usize,
    expected: &'static str,
    matches: fn(&str) -> bool,
) -> Result<(), &'static str> {
    let Some(line) = lines.get(*cursor).copied() else {
        return Err(expected);
    };
    if !matches(line) {
        return Err(expected);
    }
    *cursor += 1;
    Ok(())
}

fn line_has_uptime(line: &str) -> bool {
    line.strip_prefix("uptime: ")
        .and_then(|value| value.strip_suffix(" ms"))
        .is_some_and(|value| !value.is_empty() && value.parse::<u64>().is_ok())
}

fn line_has_ticks(line: &str) -> bool {
    line.strip_prefix("ticks: ")
        .is_some_and(|value| !value.is_empty() && value.parse::<u64>().is_ok())
}

fn line_has_memory_stats(line: &str) -> bool {
    let Some(values) = line.strip_prefix("memory: total=") else {
        return false;
    };
    let Some((total, values)) = values.split_once(" allocated=") else {
        return false;
    };
    let Some((allocated, values)) = values.split_once(" free=") else {
        return false;
    };
    let Some(free) = values.strip_suffix(" pages") else {
        return false;
    };
    [total, allocated, free]
        .into_iter()
        .all(|value| !value.is_empty() && value.parse::<usize>().is_ok())
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

    const TEST_COMMAND: &str = "'qemu-system-riscv64' '-kernel' 'kernel.elf'";

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
    fn qemu_command_line_shell_quotes_the_program_kernel_and_every_flag() {
        assert_eq!(
            qemu_command_line(Path::new("/tmp/kernel image's.elf")),
            "'qemu-system-riscv64' '-machine' 'virt' '-m' '128M' '-smp' '1' '-bios' 'default' '-kernel' '/tmp/kernel image'\\''s.elf' '-serial' 'stdio' '-monitor' 'none' '-display' 'none'"
        );
    }

    #[test]
    fn successful_boot_requires_the_exact_marker() {
        let output = "MiniOS booting...\n";

        assert_eq!(
            verify_test_result(TEST_COMMAND, TestKind::Boot, Some(0), output),
            Err(QemuError::MissingMarker {
                command: TEST_COMMAND.to_owned(),
                expected: BOOT_MARKER,
                output: output.to_owned(),
            })
        );
    }

    #[test]
    fn marker_must_be_an_exact_normalized_line() {
        for output in [
            "[MINIOS_TEST] boot: okay\n",
            "prefix [MINIOS_TEST] boot: ok\n",
            "[MINIOS_TEST] boot: ok suffix\n",
        ] {
            assert_eq!(
                verify_test_result(TEST_COMMAND, TestKind::Boot, Some(0), output),
                Err(QemuError::MissingMarker {
                    command: TEST_COMMAND.to_owned(),
                    expected: BOOT_MARKER,
                    output: output.to_owned(),
                }),
                "accepted a near-match marker: {output:?}"
            );
        }

        assert_eq!(
            verify_test_result(
                TEST_COMMAND,
                TestKind::Boot,
                Some(0),
                "firmware\r\n[MINIOS_TEST] boot: ok\r\n"
            ),
            Ok("firmware\r\n[MINIOS_TEST] boot: ok\r\n".to_owned())
        );
    }

    #[test]
    fn successful_trap_requires_the_exact_trap_marker() {
        let output = "[MINIOS_TEST] boot: ok\n";

        assert_eq!(
            verify_test_result(TEST_COMMAND, TestKind::Trap, Some(0), output),
            Err(QemuError::MissingMarker {
                command: TEST_COMMAND.to_owned(),
                expected: TRAP_MARKER,
                output: output.to_owned(),
            })
        );
    }

    #[test]
    fn successful_timer_requires_the_exact_timer_marker() {
        let output = "[MINIOS_TEST] boot: ok\n";

        assert_eq!(
            verify_test_result(TEST_COMMAND, TestKind::Timer, Some(0), output),
            Err(QemuError::MissingMarker {
                command: TEST_COMMAND.to_owned(),
                expected: TIMER_MARKER,
                output: output.to_owned(),
            })
        );
    }

    #[test]
    fn successful_memory_requires_the_exact_memory_marker() {
        let output = "[MINIOS_TEST] timer: ok\n";

        assert_eq!(
            verify_test_result(TEST_COMMAND, TestKind::Memory, Some(0), output),
            Err(QemuError::MissingMarker {
                command: TEST_COMMAND.to_owned(),
                expected: MEMORY_MARKER,
                output: output.to_owned(),
            })
        );
    }

    #[test]
    fn shell_result_rejects_each_missing_stable_output() {
        let complete = complete_shell_output();

        for missing in [
            "help      Show available commands",
            "info      Show system information",
            "uptime    Show elapsed time",
            "memory    Show physical memory statistics",
            "clear     Clear the terminal",
            "shutdown  Shut down MiniOS",
            "MiniOS 0.1.0 on RISC-V 64",
            "hart id: 0",
            "unknown command: not-a-command; try 'help'",
            "shutting down",
        ] {
            let output = complete.replace(missing, "");
            assert_eq!(
                verify_shell_result(TEST_COMMAND, Some(0), &output),
                Err(QemuError::MissingShellOutput {
                    command: TEST_COMMAND.to_owned(),
                    expected: missing,
                    output,
                })
            );
        }
    }

    #[test]
    fn shell_result_rejects_a_nonnumeric_uptime() {
        let output = complete_shell_output().replace("uptime: 10 ms", "uptime: nope ms");

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "uptime: <number> ms",
                output,
            })
        );
    }

    #[test]
    fn shell_result_requires_hart_zero_immediately_after_info() {
        let output = complete_shell_output().replace("hart id: 0\n", "");

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "hart id: 0",
                output,
            })
        );
    }

    #[test]
    fn shell_result_requires_numeric_ticks_immediately_after_uptime() {
        let output = complete_shell_output().replace("ticks: 1", "ticks: several");

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "ticks: <number>",
                output,
            })
        );
    }

    #[test]
    fn shell_result_rejects_nonnumeric_memory_stats() {
        let output = complete_shell_output().replace(
            "memory: total=32231 allocated=0 free=32231 pages",
            "memory: total=many allocated=none free=lots pages",
        );

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "memory: total=<number> allocated=<number> free=<number> pages",
                output,
            })
        );
    }

    #[test]
    fn shell_result_requires_each_prompt_and_command_echo() {
        for expected in [
            "minios> help",
            "minios> info",
            "minios> uptime",
            "minios> memory",
            "minios> not-a-command",
            "minios> shutdown",
        ] {
            let output = complete_shell_output().replace(expected, "missing prompt and echo");
            assert_eq!(
                verify_shell_result(TEST_COMMAND, Some(0), &output),
                Err(QemuError::MissingShellOutput {
                    command: TEST_COMMAND.to_owned(),
                    expected,
                    output,
                })
            );
        }
    }

    #[test]
    fn shell_result_rejects_prompt_prefix_near_matches() {
        let output = complete_shell_output().replacen("minios> help", "minios> helper", 1);

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "minios> help",
                output,
            })
        );
    }

    #[test]
    fn shell_result_rejects_out_of_order_responses() {
        let output = complete_shell_output().replace(
            "help      Show available commands\ninfo      Show system information",
            "info      Show system information\nhelp      Show available commands",
        );

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "help      Show available commands",
                output,
            })
        );
    }

    #[test]
    fn shell_result_rejects_out_of_order_commands() {
        let output = complete_shell_output().replace(
            "minios> uptime\nuptime: 10 ms\nticks: 1\nminios> memory\nmemory: total=32231 allocated=0 free=32231 pages",
            "minios> memory\nmemory: total=32231 allocated=0 free=32231 pages\nminios> uptime\nuptime: 10 ms\nticks: 1",
        );

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "minios> uptime",
                output,
            })
        );
    }

    #[test]
    fn shell_result_rejects_a_repeated_prompt() {
        let output = complete_shell_output().replace(
            "MiniOS 0.1.0 on RISC-V 64\nhart id: 0",
            "MiniOS 0.1.0 on RISC-V 64\nminios> info\nhart id: 0",
        );

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "hart id: 0",
                output,
            })
        );
    }

    #[test]
    fn shell_result_rejects_unexpected_output_inside_the_sequence() {
        let output = complete_shell_output().replace(
            "ticks: 1\nminios> memory",
            "ticks: 1\nunexpected\nminios> memory",
        );

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::MissingShellOutput {
                command: TEST_COMMAND.to_owned(),
                expected: "minios> memory",
                output,
            })
        );
    }

    #[test]
    fn shell_result_accepts_crlf_and_trailing_blank_lines() {
        let output = complete_shell_output().replace('\n', "\r\n") + "\r\n\r\n";

        assert_eq!(
            verify_shell_result(TEST_COMMAND, Some(0), &output),
            Ok(output)
        );
    }

    #[test]
    fn timeout_error_reports_the_configured_deadline() {
        let error = QemuError::TimedOut {
            command: TEST_COMMAND.to_owned(),
            deadline: Duration::from_millis(1250),
            output: "partial UART transcript".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "QEMU test timed out after 1.250 seconds:\ncommand: 'qemu-system-riscv64' '-kernel' 'kernel.elf'\npartial UART transcript"
        );
    }

    fn complete_shell_output() -> String {
        [
            "minios> help",
            "help      Show available commands",
            "info      Show system information",
            "uptime    Show elapsed time",
            "memory    Show physical memory statistics",
            "clear     Clear the terminal",
            "shutdown  Shut down MiniOS",
            "minios> info",
            "MiniOS 0.1.0 on RISC-V 64",
            "hart id: 0",
            "minios> uptime",
            "uptime: 10 ms",
            "ticks: 1",
            "minios> memory",
            "memory: total=32231 allocated=0 free=32231 pages",
            "minios> not-a-command",
            "unknown command: not-a-command; try 'help'",
            "minios> shutdown",
            "shutting down",
        ]
        .join("\n")
    }

    #[test]
    fn drains_large_stdout_and_stderr_before_the_deadline() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "yes stdout | head -c 131072; yes stderr | head -c 131072 >&2; printf '[MINIOS_TEST] boot: ok'",
        ]);

        let completed = run_command_with_capture(
            command,
            "'sh' '-c' 'large-output fixture'".to_owned(),
            Duration::from_secs(2),
        )
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

        let command_line = "'sh' '-c' 'timeout fixture'".to_owned();
        let error =
            run_command_with_capture(command, command_line.clone(), Duration::from_millis(50))
                .expect_err("sleeping process must time out");
        let display = error.to_string();
        let output = match error {
            QemuError::TimedOut {
                command, output, ..
            } => {
                assert_eq!(command, command_line);
                output
            }
            other => panic!("expected timeout, got {other:?}"),
        };

        assert!(output.contains("stdout-before-timeout"));
        assert!(output.contains("stderr-before-timeout"));
        assert!(display.contains("command: 'sh' '-c' 'timeout fixture'"));
        assert!(display.contains("stdout-before-timeout"));
        assert!(display.contains("stderr-before-timeout"));
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
