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
use minios_abi::control::{FRAME_HEADER_LEN, FrameHeader};

const QEMU_PROGRAM: &str = "qemu-system-riscv64";
const BOOT_MARKER: &str = "[MINIOS_TEST] boot: ok";
const TIMER_MARKER: &str = "[MINIOS_TEST] timer: ok";
const TRAP_MARKER: &str = "[MINIOS_TEST] trap: ok";
const MEMORY_MARKER: &str = "[MINIOS_TEST] memory: ok";
const VM_MARKER: &str = "[MINIOS_TEST] vm: ok";
const ELF_MARKER: &str = "[MINIOS_TEST] elf: ok";
const USER_ENTRY_MARKER: &str = "[MINIOS_TEST] user-entry: reached";
const USER_TRAP_REJECTED_MARKER: &str = "[MINIOS_TEST] user-trap: rejected";
const USER_TRAP_OK_MARKER: &str = "[MINIOS_TEST] user-trap: ok";
const USER_SYSCALL_MARKER: &str = "[MINIOS_TEST] user-syscall: ok";
const USER_EXIT_MARKER: &str = "[MINIOS_TEST] user-exit: ok code=42";
const PAYLOAD_READY_FRAME: &[u8] = b"MCF1\x01\0\0\0\x04\0\0\0\x01\0\0\0";
const PAYLOAD_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x03\0\0\0MK6";
const PAYLOAD_STDERR_FRAME: &[u8] = b"MCF1\x03\0\0\0\x03\0\0\0MK6";
const PAYLOAD_EXIT_FRAME: &[u8] = b"MCF1\x04\0\0\0\x04\0\0\0\x2a\0\0\0";
const PAYLOAD_DIAGNOSTIC_FRAME: &[u8] = b"MCF1\x06\0\0\0\x1d\0\0\0\r\nMiniOS payload: ok code=42\n";
const USER_EXIT_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x03\0\0\0MK5";
const USER_EXIT_STDERR_FRAME: &[u8] = b"MCF1\x03\0\0\0\x03\0\0\0MK5";
const USER_EXIT_CONTROL_FRAME: &[u8] = b"MCF1\x04\0\0\0\x04\0\0\0\x2a\0\0\0";
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
    Vm,
    Elf,
    UserEntry,
    UserTrap,
    UserSyscall,
    UserExit,
    Payload,
    Shell,
}

impl TestKind {
    fn feature(self) -> &'static str {
        match self {
            Self::Boot => "qemu-test-boot",
            Self::Timer => "qemu-test-timer",
            Self::Trap => "qemu-test-trap",
            Self::Memory => "qemu-test-memory",
            Self::Vm => "qemu-test-vm",
            Self::Elf => "qemu-test-elf",
            Self::UserEntry => "qemu-test-user-entry",
            Self::UserTrap => "qemu-test-user-trap",
            Self::UserSyscall => "qemu-test-user-syscall",
            Self::UserExit => "qemu-test-user-exit",
            Self::Payload => unreachable!("the payload test boots the normal kernel"),
            Self::Shell => unreachable!("the shell test boots the normal kernel"),
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::Boot => BOOT_MARKER,
            Self::Timer => TIMER_MARKER,
            Self::Trap => TRAP_MARKER,
            Self::Memory => MEMORY_MARKER,
            Self::Vm => VM_MARKER,
            Self::Elf => ELF_MARKER,
            Self::UserEntry => USER_ENTRY_MARKER,
            Self::UserTrap => USER_TRAP_REJECTED_MARKER,
            Self::UserSyscall => USER_SYSCALL_MARKER,
            Self::UserExit => USER_EXIT_MARKER,
            Self::Payload => unreachable!("the payload test verifies raw control frames"),
            Self::Shell => unreachable!("the shell test verifies an interactive transcript"),
        }
    }

    /// A marker that must never appear, proving a rejection test cannot
    /// masquerade as its success counterpart.
    fn forbidden_marker(self) -> Option<&'static str> {
        match self {
            Self::UserTrap => Some(USER_TRAP_OK_MARKER),
            _ => None,
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
    ForbiddenMarker {
        command: String,
        forbidden: &'static str,
        output: String,
    },
    PayloadFrames {
        command: String,
        output: String,
    },
    MissingControlFrame {
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
            Self::ForbiddenMarker {
                command,
                forbidden,
                output,
            } => write!(
                formatter,
                "QEMU test printed the forbidden marker {forbidden}:\ncommand: {command}\n{}",
                output.trim_end()
            ),
            Self::PayloadFrames { command, output } => write!(
                formatter,
                "QEMU payload run did not emit the expected Ready/stdout/stderr/Exit/cleanup frame sequence:\ncommand: {command}\n{}",
                output.trim_end()
            ),
            Self::MissingControlFrame {
                command,
                expected,
                output,
            } => write!(
                formatter,
                "QEMU user-exit output did not contain the ordered {expected}:\ncommand: {command}\n{}",
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
    if kind == TestKind::Payload {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create()?;
        let (command, command_line) = qemu_command_with_payload(&kernel, bundle.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        return verify_payload_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::Shell {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let (command, command_line) = qemu_command(&kernel);
        let completed = run_shell_command(command, command_line.clone(), deadline)?;
        return verify_shell_result(&command_line, completed.status.code(), &completed.output);
    }

    let kernel = cargo::build_kernel_for_test(kind.feature()).map_err(QemuError::Build)?;
    let (command, command_line) = qemu_command(&kernel);
    run_marker_test(kind, command, command_line, deadline)
}

fn run_marker_test(
    kind: TestKind,
    command: Command,
    command_line: String,
    deadline: Duration,
) -> Result<String, QemuError> {
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

fn qemu_command_with_payload(kernel: &Path, bundle: &Path) -> (Command, String) {
    let mut args = qemu_args(kernel);
    args.push("-device".to_owned());
    args.push(format!(
        "loader,file={},addr=0x87800000,force-raw=on",
        bundle.display()
    ));
    let command_line = render_command(QEMU_PROGRAM, &args);
    let mut command = Command::new(QEMU_PROGRAM);
    command.args(&args);
    (command, command_line)
}

/// payload検査で期待されるcontrol frame列 (Ready→stdout→stderr→Exit→cleanup)。
#[cfg(test)]
fn expected_payload_frames() -> Vec<u8> {
    let mut expected = Vec::new();
    expected.extend_from_slice(PAYLOAD_READY_FRAME);
    expected.extend_from_slice(PAYLOAD_STDOUT_FRAME);
    expected.extend_from_slice(PAYLOAD_STDERR_FRAME);
    expected.extend_from_slice(PAYLOAD_EXIT_FRAME);
    expected.extend_from_slice(PAYLOAD_DIAGNOSTIC_FRAME);
    expected
}

fn verify_payload_result(
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
    let output_bytes = output.as_bytes();
    if !has_exact_payload_frames(output_bytes) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

/// Ready前のfirmware出力を許可し、Readyから出力末尾までをpayload control frameとして
/// 完全に消費する。
fn has_exact_payload_frames(output: &[u8]) -> bool {
    let Some(start) = output
        .windows(PAYLOAD_READY_FRAME.len())
        .position(|window| window == PAYLOAD_READY_FRAME)
    else {
        return false;
    };
    let mut remaining = &output[start..];

    let expected_frames = [
        PAYLOAD_READY_FRAME,
        PAYLOAD_STDOUT_FRAME,
        PAYLOAD_STDERR_FRAME,
        PAYLOAD_EXIT_FRAME,
        PAYLOAD_DIAGNOSTIC_FRAME,
    ];
    let mut expected_index = 0;

    while !remaining.is_empty() {
        let Some(header_bytes) = remaining.get(..FRAME_HEADER_LEN) else {
            return false;
        };
        let Ok(header) = FrameHeader::decode(header_bytes) else {
            return false;
        };
        let Ok(payload_len) = usize::try_from(header.payload_len) else {
            return false;
        };
        let Some(frame) = remaining.get(..FRAME_HEADER_LEN + payload_len) else {
            return false;
        };
        let Some(expected) = expected_frames.get(expected_index) else {
            return false;
        };
        if frame != *expected {
            return false;
        }
        expected_index += 1;
        remaining = &remaining[frame.len()..];
    }

    expected_index == expected_frames.len()
}

/// payload検査用の一時MiniBundle file。生成時に書き込み、removeで必ず消す。
struct PayloadBundle {
    path: std::path::PathBuf,
}

impl PayloadBundle {
    fn create() -> Result<Self, QemuError> {
        let path = std::env::temp_dir().join(format!(
            "minios-payload-{}-{}.mcb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos()
        ));
        std::fs::write(&path, payload_bundle_bytes()).map_err(|error| QemuError::Spawn {
            command: path.display().to_string(),
            error: error.to_string(),
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for PayloadBundle {
    fn drop(&mut self) {
        self.remove();
    }
}

/// kernelに渡す決定的なpayload MiniBundle (manifest "hello" + payload ELF)。
fn payload_bundle_bytes() -> Vec<u8> {
    const MANIFEST: &[u8] = b"version=1\nname=hello\n";
    let elf = payload_elf_bytes();
    let manifest_end = 96 + MANIFEST.len();
    let padding_len = (8 - manifest_end % 8) % 8;
    let elf_offset = manifest_end + padding_len;
    let total_len = elf_offset + elf.len();

    let mut header = [0u8; 96];
    header[0..8].copy_from_slice(b"MINICTR\0");
    header[8..10].copy_from_slice(&1u16.to_le_bytes());
    header[12..14].copy_from_slice(&96u16.to_le_bytes());
    header[16..24].copy_from_slice(&(total_len as u64).to_le_bytes());
    header[24..32].copy_from_slice(&(96u64).to_le_bytes());
    header[32..40].copy_from_slice(&(MANIFEST.len() as u64).to_le_bytes());
    header[40..48].copy_from_slice(&(elf_offset as u64).to_le_bytes());
    header[48..56].copy_from_slice(&(elf.len() as u64).to_le_bytes());

    let mut bytes = vec![0u8; total_len];
    bytes[..96].copy_from_slice(&header);
    bytes[96..manifest_end].copy_from_slice(MANIFEST);
    bytes[elf_offset..].copy_from_slice(&elf);
    // digest = SHA-256(digest fieldを0にしたheader || header以降の可変bytes)。
    let mut digest_input = Vec::with_capacity(96 + total_len - 96);
    let mut zeroed_header = header;
    zeroed_header[56..88].fill(0);
    digest_input.extend_from_slice(&zeroed_header);
    digest_input.extend_from_slice(&bytes[96..]);
    bytes[56..88].copy_from_slice(&sha256(&digest_input));
    bytes
}

/// payload ELF: stdout "MK6"、stderr "MK6"、exit(42)を順に発行するだけの
/// 決定的な1 segment RV64実行fileである。
fn payload_elf_bytes() -> Vec<u8> {
    const X0: u32 = 0;
    const SP: u32 = 2;
    const T0: u32 = 5;
    const S0: u32 = 8;
    const A0: u32 = 10;
    const A1: u32 = 11;
    const A2: u32 = 12;
    const A7: u32 = 17;
    let addi = |rd: u32, rs1: u32, imm: i16| {
        (((imm as u32) & 0xfff) << 20) | (rs1 << 15) | (rd << 7) | 0x0013
    };
    let sb = |rs2: u32, rs1: u32, imm: i16| {
        let imm = imm as u32;
        (((imm >> 5) & 0x7f) << 25) | (rs2 << 20) | (rs1 << 15) | ((imm & 0x1f) << 7) | 0x0023
    };
    let ecall = || 0x0000_0073u32;

    let mut code: Vec<u32> = Vec::new();
    code.push(addi(S0, SP, -64));
    for (offset, byte) in [(0_i16, 0x4d_i16), (1, 0x4b), (2, 0x36)] {
        code.push(addi(T0, X0, byte));
        code.push(sb(T0, S0, offset));
    }
    for descriptor in [1_u32, 2] {
        code.push(addi(A0, X0, descriptor as i16));
        code.push(addi(A1, S0, 0));
        code.push(addi(A2, X0, 3));
        code.push(addi(A7, X0, 1));
        code.push(ecall());
    }
    // exit(42)
    code.push(addi(A0, X0, 42));
    code.push(addi(A7, X0, 2));
    code.push(ecall());
    // 到達しない安全ループ
    code.push(0x0000_006f);

    let code_bytes: Vec<u8> = code.iter().flat_map(|word| word.to_le_bytes()).collect();
    let elf_len = 0x1000 + code_bytes.len();
    let mut bytes = vec![0u8; elf_len];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&243u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x0010_0000u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    let header = 64;
    bytes[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
    bytes[header + 4..header + 8].copy_from_slice(&5u32.to_le_bytes());
    bytes[header + 8..header + 16].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[header + 16..header + 24].copy_from_slice(&0x0010_0000u64.to_le_bytes());
    bytes[header + 32..header + 40].copy_from_slice(&(code_bytes.len() as u64).to_le_bytes());
    bytes[header + 40..header + 48].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[header + 48..header + 56].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[0x1000..].copy_from_slice(&code_bytes);
    bytes
}

/// SHA-256 (FIPS 180-4)。外部crateを追加せずにpayload digestを計算するための
/// 最小実装である。
mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    pub fn digest(message: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let bit_len = (message.len() as u64).wrapping_mul(8);
        let mut padded = message.to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&bit_len.to_be_bytes());

        for block in padded.chunks(64) {
            let mut w = [0u32; 64];
            for (index, word) in block.chunks(4).enumerate() {
                w[index] = u32::from_be_bytes(word.try_into().expect("4-byte chunk"));
            }
            for index in 16..64 {
                let s0 = w[index - 15].rotate_right(7)
                    ^ w[index - 15].rotate_right(18)
                    ^ (w[index - 15] >> 3);
                let s1 = w[index - 2].rotate_right(17)
                    ^ w[index - 2].rotate_right(19)
                    ^ (w[index - 2] >> 10);
                w[index] = w[index - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[index - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for index in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let temp1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[index])
                    .wrapping_add(w[index]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let temp2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(temp1);
                d = c;
                c = b;
                b = a;
                a = temp1.wrapping_add(temp2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }
        let mut digest = [0u8; 32];
        for (index, word) in h.iter().enumerate() {
            digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

fn sha256(message: &[u8]) -> [u8; 32] {
    sha256::digest(message)
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
    if let Some(forbidden) = kind.forbidden_marker()
        && normalized.lines().any(|line| line == forbidden)
    {
        return Err(QemuError::ForbiddenMarker {
            command: command.to_owned(),
            forbidden,
            output: output.to_owned(),
        });
    }
    if kind == TestKind::UserExit {
        verify_user_exit_control_frames(command, output)?;
    }
    Ok(output.to_owned())
}

fn verify_user_exit_control_frames(command: &str, output: &str) -> Result<(), QemuError> {
    let mut remaining = output.as_bytes();
    for (expected, frame) in [
        ("Stdout(MK5) frame", USER_EXIT_STDOUT_FRAME),
        ("Stderr(MK5) frame", USER_EXIT_STDERR_FRAME),
        ("Exit(42) frame", USER_EXIT_CONTROL_FRAME),
    ] {
        let Some(position) = remaining
            .windows(frame.len())
            .position(|bytes| bytes == frame)
        else {
            return Err(QemuError::MissingControlFrame {
                command: command.to_owned(),
                expected,
                output: output.to_owned(),
            });
        };
        remaining = &remaining[position + frame.len()..];
    }
    Ok(())
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
    fn vm_and_elf_tests_select_their_features_and_exact_markers() {
        assert_eq!(TestKind::Vm.feature(), "qemu-test-vm");
        assert_eq!(TestKind::Vm.marker(), "[MINIOS_TEST] vm: ok");
        assert_eq!(TestKind::Elf.feature(), "qemu-test-elf");
        assert_eq!(TestKind::Elf.marker(), "[MINIOS_TEST] elf: ok");
        assert_eq!(TestKind::UserEntry.feature(), "qemu-test-user-entry");
        assert_eq!(
            TestKind::UserEntry.marker(),
            "[MINIOS_TEST] user-entry: reached"
        );
        assert_eq!(TestKind::UserTrap.feature(), "qemu-test-user-trap");
        assert_eq!(
            TestKind::UserTrap.marker(),
            "[MINIOS_TEST] user-trap: rejected"
        );
        assert_eq!(
            TestKind::UserTrap.forbidden_marker(),
            Some("[MINIOS_TEST] user-trap: ok")
        );
        assert_eq!(TestKind::UserSyscall.feature(), "qemu-test-user-syscall");
        assert_eq!(
            TestKind::UserSyscall.marker(),
            "[MINIOS_TEST] user-syscall: ok"
        );
        assert_eq!(TestKind::UserExit.feature(), "qemu-test-user-exit");
        assert_eq!(
            TestKind::UserExit.marker(),
            "[MINIOS_TEST] user-exit: ok code=42"
        );
        for kind in [
            TestKind::Boot,
            TestKind::Timer,
            TestKind::Trap,
            TestKind::Memory,
            TestKind::Vm,
            TestKind::Elf,
            TestKind::UserEntry,
            TestKind::UserSyscall,
            TestKind::UserExit,
            TestKind::Shell,
        ] {
            assert_eq!(kind.forbidden_marker(), None);
        }
    }

    // Catches a rejection test that resumes the guest and reports success
    // alongside the expected diagnostic.
    #[test]
    fn user_trap_rejection_requires_the_marker_and_forbids_the_success_marker() {
        let rejected = "OpenSBI\r\n[MINIOS_TEST] user-trap: rejected\r\n";

        assert_eq!(
            verify_test_result(TEST_COMMAND, TestKind::UserTrap, Some(0), rejected),
            Ok(rejected.to_owned())
        );
        assert_eq!(
            verify_test_result(
                TEST_COMMAND,
                TestKind::UserTrap,
                Some(0),
                "[MINIOS_TEST] user-trap: rejected\r\n[MINIOS_TEST] user-trap: ok\r\n"
            ),
            Err(QemuError::ForbiddenMarker {
                command: TEST_COMMAND.to_owned(),
                forbidden: USER_TRAP_OK_MARKER,
                output: "[MINIOS_TEST] user-trap: rejected\r\n[MINIOS_TEST] user-trap: ok\r\n"
                    .to_owned(),
            })
        );
        assert_eq!(
            verify_test_result(
                TEST_COMMAND,
                TestKind::UserTrap,
                Some(0),
                "[MINIOS_TEST] user-trap: rejected suffix\r\n"
            ),
            Err(QemuError::MissingMarker {
                command: TEST_COMMAND.to_owned(),
                expected: USER_TRAP_REJECTED_MARKER,
                output: "[MINIOS_TEST] user-trap: rejected suffix\r\n".to_owned(),
            })
        );
        assert_eq!(
            verify_test_result(TEST_COMMAND, TestKind::UserTrap, Some(1), rejected),
            Err(QemuError::Failed {
                command: TEST_COMMAND.to_owned(),
                status: Some(1),
                output: rejected.to_owned(),
            })
        );
    }

    // Catches accepting the cleanup marker when the guest write calls failed
    // and only the final Exit frame reached the UART.
    #[test]
    fn user_exit_requires_stdout_stderr_and_exit_control_frames() {
        let output =
            "OpenSBI\nMCF1\u{4}\0\0\0\u{4}\0\0\0*\0\0\0\r\n[MINIOS_TEST] user-exit: ok code=42\r\n";

        assert!(
            verify_test_result(TEST_COMMAND, TestKind::UserExit, Some(0), output).is_err(),
            "the marker alone must not hide missing stdout/stderr frames"
        );

        let complete = concat!(
            "OpenSBI\n",
            "MCF1\u{2}\0\0\0\u{3}\0\0\0MK5",
            "MCF1\u{3}\0\0\0\u{3}\0\0\0MK5",
            "MCF1\u{4}\0\0\0\u{4}\0\0\0*\0\0\0",
            "\r\n[MINIOS_TEST] user-exit: ok code=42\r\n"
        );
        assert_eq!(
            verify_test_result(TEST_COMMAND, TestKind::UserExit, Some(0), complete),
            Ok(complete.to_owned())
        );
    }

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

    // Catches a broken SHA-256 round (message schedule or padding drift)
    // silently producing a digest that real importers would reject.
    #[test]
    fn sha256_matches_the_known_vectors() {
        assert_eq!(
            sha256(b""),
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
                0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
                0x78, 0x52, 0xb8, 0x55,
            ]
        );
        assert_eq!(
            sha256(b"abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    // Catches a bundle whose header, manifest, padding, ELF placement, or
    // digest drifts from the canonical layout the kernel parser validates.
    #[test]
    fn payload_bundle_is_canonical_and_self_consistent() {
        let bundle = payload_bundle_bytes();
        assert_eq!(&bundle[0..8], b"MINICTR\0");
        assert_eq!(&bundle[8..10], &1u16.to_le_bytes());
        let total_len = u64::from_le_bytes(bundle[16..24].try_into().unwrap());
        assert_eq!(total_len as usize, bundle.len());
        let manifest_len = u64::from_le_bytes(bundle[32..40].try_into().unwrap()) as usize;
        assert_eq!(&bundle[96..96 + manifest_len], b"version=1\nname=hello\n");
        let manifest_end = 96 + manifest_len;
        let elf_offset = u64::from_le_bytes(bundle[40..48].try_into().unwrap()) as usize;
        assert_eq!(elf_offset, manifest_end + (8 - manifest_end % 8) % 8);
        assert_eq!(
            bundle[manifest_end..elf_offset],
            vec![0u8; elf_offset - manifest_end]
        );
        assert!(bundle[elf_offset..].starts_with(b"\x7fELF"));
        assert_eq!(
            u64::from_le_bytes(bundle[48..56].try_into().unwrap()),
            (bundle.len() - elf_offset) as u64
        );

        // digestを自力で再計算し、headerのdigest fieldと一致することへ確認する。
        let mut digest_input = Vec::new();
        let mut zeroed = bundle[..96].to_vec();
        zeroed[56..88].fill(0);
        digest_input.extend_from_slice(&zeroed);
        digest_input.extend_from_slice(&bundle[96..]);
        assert_eq!(&bundle[56..88], &sha256(&digest_input)[..]);
    }

    // Catches a payload ELF that stops emitting stdout/stderr/exit or changes
    // the written bytes the frame assertions depend on.
    #[test]
    fn payload_elf_is_deterministic_and_minimal() {
        assert_eq!(payload_elf_bytes(), payload_elf_bytes());
        assert!(payload_elf_bytes().starts_with(b"\x7fELF"));
        assert_eq!(payload_elf_bytes().len(), 0x1000 + 21 * 4);
        // 最後の命令はecall (exit) であり、その手前はa7=2 (Exit) の設定である。
        let code = &payload_elf_bytes()[0x1000..];
        // 最後は安全ループ (jal x0, 0)、その手前がecall (exit) である。
        assert_eq!(&code[code.len() - 4..], &0x0000_006fu32.to_le_bytes());
        assert_eq!(
            &code[code.len() - 8..code.len() - 4],
            &0x0000_0073u32.to_le_bytes()
        );
    }

    // Catches a loader argument drift that would place the bundle outside the
    // reserved window the kernel validates.
    #[test]
    fn payload_qemu_command_carries_the_reserved_window_loader() {
        let (command, command_line) =
            qemu_command_with_payload(Path::new("kernel.elf"), Path::new("/tmp/hello.mcb"));
        let args: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert!(contains_pair(
            &args,
            "-device",
            "loader,file=/tmp/hello.mcb,addr=0x87800000,force-raw=on"
        ));
        assert!(command_line.contains("loader,file=/tmp/hello.mcb"));
    }

    // Catches a payload run that misses any of the five control frames or
    // reorders them.
    #[test]
    fn payload_verification_requires_the_exact_frame_sequence() {
        let mut output = "OpenSBI\n[ok] traps\n".to_owned();
        for frame in expected_payload_frames() {
            output.push(frame as char);
        }
        assert_eq!(
            verify_payload_result(TEST_COMMAND, Some(0), &output).map(|_| ()),
            Ok(())
        );

        let truncated = {
            let expected = expected_payload_frames();
            let mut output = String::from("boot\n");
            output.push_str(&String::from_utf8_lossy(&expected[..expected.len() - 4]));
            output
        };
        assert!(matches!(
            verify_payload_result(TEST_COMMAND, Some(0), &truncated),
            Err(QemuError::PayloadFrames { .. })
        ));
        assert!(matches!(
            verify_payload_result(TEST_COMMAND, Some(1), &output),
            Err(QemuError::Failed { .. })
        ));
    }

    // Catches accepting UART text after Ready, where the payload contract
    // requires every remaining byte to belong to a control frame.
    #[test]
    fn payload_verification_rejects_plain_uart_after_ready() {
        let mut output = complete_payload_output();
        output.push('!');

        assert!(matches!(
            verify_payload_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::PayloadFrames { .. })
        ));
    }

    // Catches accepting a complete but unsupported control frame after the
    // expected payload result.
    #[test]
    fn payload_verification_rejects_unknown_frame_after_ready() {
        let mut output = complete_payload_output();
        output.push_str("MCF1\x7f\0\0\0\0\0\0\0");

        assert!(matches!(
            verify_payload_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::PayloadFrames { .. })
        ));
    }

    // Catches accepting a second Ready frame after the expected sequence.
    #[test]
    fn payload_verification_rejects_out_of_order_frame_after_ready() {
        let mut output = complete_payload_output();
        output.push_str(std::str::from_utf8(PAYLOAD_READY_FRAME).unwrap());

        assert!(matches!(
            verify_payload_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::PayloadFrames { .. })
        ));
    }

    // Catches accepting a partial header that leaves unconsumed bytes after
    // the expected frame sequence.
    #[test]
    fn payload_verification_rejects_truncated_frame_after_ready() {
        let mut output = complete_payload_output();
        output.push_str("MCF1\x02");

        assert!(matches!(
            verify_payload_result(TEST_COMMAND, Some(0), &output),
            Err(QemuError::PayloadFrames { .. })
        ));
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

    // Catches the user-trap path bypassing the shared timeout cleanup after
    // emitting its expected rejection diagnostic.
    #[test]
    fn user_trap_harness_reaps_a_timed_out_negative_result() {
        let pid_file = std::env::temp_dir().join(format!(
            "minios-user-trap-pid-{}-{}",
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
                "echo $$ > '{}'; printf '{}'; exec sleep 30",
                shell_quote_path(&pid_file),
                USER_TRAP_REJECTED_MARKER,
            ),
        ]);

        let command_line = "'sh' '-c' 'user-trap timeout fixture'".to_owned();
        let error = run_marker_test(
            TestKind::UserTrap,
            command,
            command_line.clone(),
            Duration::from_millis(50),
        )
        .expect_err("the sleeping user-trap fixture must time out");
        let output = match error {
            QemuError::TimedOut {
                command, output, ..
            } => {
                assert_eq!(command, command_line);
                output
            }
            other => panic!("expected timeout, got {other:?}"),
        };

        assert!(output.contains(USER_TRAP_REJECTED_MARKER));
        assert!(!output.contains(USER_TRAP_OK_MARKER));
        let pid = fs::read_to_string(&pid_file).expect("timed-out process must record its PID");
        let status = Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("kill must start");
        let _ = fs::remove_file(&pid_file);
        assert!(
            !status.success(),
            "the timed-out user-test child must be killed and reaped"
        );
    }

    fn complete_payload_output() -> String {
        let mut output = "OpenSBI\n[ok] traps\n".to_owned();
        for frame in expected_payload_frames() {
            output.push(frame as char);
        }
        output
    }

    fn shell_quote_path(path: &Path) -> String {
        path.display().to_string().replace('\'', "'\\''")
    }
}
