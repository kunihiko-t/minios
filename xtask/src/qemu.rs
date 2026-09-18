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
use minios_abi::control::{FRAME_HEADER_LEN, FrameHeader, FrameKind, ProcExitPayload};

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
const USER_TRAP_FAULT_DIAGNOSTIC: &str =
    "MiniOS user trap: scause=0x000000000000000f stval=0x0000000010000000";
const USER_SYSCALL_MARKER: &str = "[MINIOS_TEST] user-syscall: ok";
const USER_EXIT_MARKER: &str = "[MINIOS_TEST] user-exit: ok code=42";
// QEMU `virt` -m 128Mが渡すDTBからkernelが発見するmachine記述の期待値。
const FDT_MARKER: &str =
    "[MINIOS_TEST] fdt: ram=0x80000000..0x88000000 uart=0x10000000 timebase=10000000";
// `-m 128M`ではヒープ領域は`0x8770_0000..0x8780_0000`の1 MiBである。
const HEAP_MARKER: &str = "[MINIOS_TEST] heap: ok";
const VIRTIO_MARKER: &str = "[MINIOS_TEST] virtio: ok";
const PAYLOAD_READY_FRAME: &[u8] = b"MCF1\x01\0\0\0\x04\0\0\0\x01\0\x02\0";
/// Ready frameと同じbyte列の`&str`。live出力のwindow照合で待つ。
const PAYLOAD_READY_TEXT: &str = "MCF1\x01\0\0\0\x04\0\0\0\x01\0\x02\0";
/// payload-stdin検査の入力。2 frameのbyte列とEOFのStdin frameである。
const STDIN_TEST_FRAMES: &[u8] =
    b"MCF1\x07\0\0\0\x02\0\0\0abMCF1\x07\0\0\0\x04\0\0\0cdefMCF1\x07\0\0\0\0\0\0\0";
const STDIN_STDOUT_AB_FRAME: &[u8] = b"MCF1\x02\0\0\0\x02\0\0\0ab";
const STDIN_STDOUT_CDEF_FRAME: &[u8] = b"MCF1\x02\0\0\0\x04\0\0\0cdef";
const PAYLOAD_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x03\0\0\0MK6";
const PAYLOAD_STDERR_FRAME: &[u8] = b"MCF1\x03\0\0\0\x03\0\0\0MK6";
const PAYLOAD_EXIT_FRAME: &[u8] = b"MCF1\x04\0\0\0\x04\0\0\0\x2a\0\0\0";
const PAYLOAD_DIAGNOSTIC_FRAME: &[u8] = b"MCF1\x06\0\0\0\x1d\0\0\0\r\nMiniOS payload: ok code=42\n";
const PAYLOAD_SPAWNED_HELLO_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x27\0\0\0MiniOS sched: spawned pid=0 name=hello\n";
const PAYLOAD_SPAWNED_STDIN_CAT_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2b\0\0\0MiniOS sched: spawned pid=0 name=stdin-cat\n";
const FILE_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2b\0\0\0MiniOS sched: spawned pid=0 name=file-read\n";
const FILE_FD_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x29\0\0\0MiniOS sched: spawned pid=0 name=file-fd\n";
const FILE_WRITE_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2c\0\0\0MiniOS sched: spawned pid=0 name=file-write\n";
const FILE_UNLINK_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2d\0\0\0MiniOS sched: spawned pid=0 name=file-unlink\n";
const FILE_SEEK_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2b\0\0\0MiniOS sched: spawned pid=0 name=file-seek\n";
const FILE_RENAME_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2d\0\0\0MiniOS sched: spawned pid=0 name=file-rename\n";
const FILE_MKDIR_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2c\0\0\0MiniOS sched: spawned pid=0 name=file-mkdir\n";
const FILE_SPAWN_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2c\0\0\0MiniOS sched: spawned pid=0 name=file-spawn\n";
const FILE_WAITPID_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2e\0\0\0MiniOS sched: spawned pid=0 name=file-waitpid\n";
const FILE_STAT_SPAWNED_FRAME: &[u8] =
    b"MCF1\x06\0\0\0\x2b\0\0\0MiniOS sched: spawned pid=0 name=file-stat\n";
const FILE_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x11\0\0\0note inside docs\n";
const FILE_WRITE_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x11\0\0\0written by guest\n";
const FILE_UNLINK_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x0d\0\0\0file removed\n";
const FILE_SEEK_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x0e\0\0\0seek verified\n";
const FILE_RENAME_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x10\0\0\0rename verified\n";
const FILE_MKDIR_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x0f\0\0\0mkdir verified\n";
const FILE_SPAWN_PARENT_STDOUT: &[u8] = b"spawn verified\n";
const FILE_SPAWN_CHILD_STDOUT: &[u8] = b"spawn-child\n";
const FILE_WAITPID_CHILD_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x0c\0\0\0spawn-child\n";
const FILE_WAITPID_PARENT_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x11\0\0\0waitpid verified\n";
const FILE_STAT_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x0e\0\0\0stat verified\n";
const ARGS_STDOUT_HELLO_FRAME: &[u8] = b"MCF1\x02\0\0\0\x05\0\0\0hello";
const ARGS_STDOUT_ALPHA_FRAME: &[u8] = b"MCF1\x02\0\0\0\x05\0\0\0alpha";
const ARGS_STDOUT_BRAVO_FRAME: &[u8] = b"MCF1\x02\0\0\0\x05\0\0\0bravo";
const USER_EXIT_STDOUT_FRAME: &[u8] = b"MCF1\x02\0\0\0\x03\0\0\0MK5";
const USER_EXIT_STDERR_FRAME: &[u8] = b"MCF1\x03\0\0\0\x03\0\0\0MK5";
const USER_EXIT_CONTROL_FRAME: &[u8] = b"MCF1\x04\0\0\0\x04\0\0\0\x2a\0\0\0";
const SHELL_PROMPT: &str = "minios> ";
const SHELL_SCRIPT: &[u8] =
    b"help\ninfo\nuptime\nmemory\nls\nls DOCS\ncat DOCS/NOTE.TXT\ncat Long File Name.txt\nrm Long File Name.txt\nls\ncat Long File Name.txt\nmkdir NEWDIR\nls\nrmdir DOCS\nrmdir NEWDIR\nls\nmv HELLO.TXT WORLD.TXT\nls\nmv WORLD.TXT HELLO.TXT\nmv DOCS NOTESD\nls\ncat NOTESD/NOTE.TXT\nmv NOTESD DOCS\nmv HELLO.TXT DOCS/MOVED.TXT\nls\ncat DOCS/MOVED.TXT\nmv DOCS/MOVED.TXT HELLO.TXT\nnot-a-command\nshutdown\n";
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
    Fdt,
    Heap,
    Virtio,
    Payload,
    File,
    FileFd,
    FileWrite,
    FileUnlink,
    FileSeek,
    FileRename,
    FileMkdir,
    FileSpawn,
    FileWaitpid,
    FileStat,
    PayloadArgs,
    PayloadStdin,
    Sched,
    SchedIo,
    SchedIoPartial,
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
            Self::Fdt => "qemu-test-fdt",
            Self::Heap => "qemu-test-heap",
            Self::Virtio => "qemu-test-virtio",
            Self::Payload => unreachable!("the payload test boots the normal kernel"),
            Self::File => unreachable!("the file test boots the normal kernel"),
            Self::FileFd => unreachable!("the file-fd test boots the normal kernel"),
            Self::FileWrite => unreachable!("the file-write test boots the normal kernel"),
            Self::FileUnlink => {
                unreachable!("the file-unlink test boots the normal kernel")
            }
            Self::FileSeek => {
                unreachable!("the file-seek test boots the normal kernel")
            }
            Self::FileRename => {
                unreachable!("the file-rename test boots the normal kernel")
            }
            Self::FileMkdir => {
                unreachable!("the file-mkdir test boots the normal kernel")
            }
            Self::FileSpawn => {
                unreachable!("the file-spawn test boots the normal kernel")
            }
            Self::FileWaitpid => {
                unreachable!("the file-waitpid test boots the normal kernel")
            }
            Self::FileStat => {
                unreachable!("the file-stat test boots the normal kernel")
            }
            Self::PayloadArgs => unreachable!("the payload-args test boots the normal kernel"),
            Self::PayloadStdin => unreachable!("the payload-stdin test boots the normal kernel"),
            Self::Sched => unreachable!("the sched test boots the normal kernel"),
            Self::SchedIo => unreachable!("the sched-io test boots the normal kernel"),
            Self::SchedIoPartial => {
                unreachable!("the sched-io-partial test boots the normal kernel")
            }
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
            Self::Fdt => FDT_MARKER,
            Self::Heap => HEAP_MARKER,
            Self::Virtio => VIRTIO_MARKER,
            Self::Payload => unreachable!("the payload test verifies raw control frames"),
            Self::File => unreachable!("the file test verifies raw control frames"),
            Self::FileFd => unreachable!("the file-fd test verifies raw control frames"),
            Self::FileWrite => {
                unreachable!("the file-write test verifies raw control frames")
            }
            Self::FileUnlink => {
                unreachable!("the file-unlink test verifies raw control frames")
            }
            Self::FileSeek => {
                unreachable!("the file-seek test verifies raw control frames")
            }
            Self::FileRename => {
                unreachable!("the file-rename test verifies raw control frames")
            }
            Self::FileMkdir => {
                unreachable!("the file-mkdir test verifies raw control frames")
            }
            Self::FileSpawn => {
                unreachable!("the file-spawn test verifies interleaved control frames")
            }
            Self::FileWaitpid => {
                unreachable!("the file-waitpid test verifies interleaved control frames")
            }
            Self::FileStat => {
                unreachable!("the file-stat test verifies raw control frames")
            }
            Self::PayloadArgs => unreachable!("the payload-args test verifies raw control frames"),
            Self::PayloadStdin => {
                unreachable!("the payload-stdin test verifies raw control frames")
            }
            Self::Sched => unreachable!("the sched test verifies interleaved control frames"),
            Self::SchedIo => {
                unreachable!("the sched-io test verifies a blocked reader's control frames")
            }
            Self::SchedIoPartial => {
                unreachable!("the sched-io-partial test verifies a split frame's control frames")
            }
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
    Bundle {
        stage: &'static str,
        error: String,
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
            Self::Bundle { stage, error } => write!(
                formatter,
                "could not build the QEMU test bundle ({stage}):\n{}",
                error.trim_end()
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
                "QEMU payload run did not emit the expected control-frame sequence:\ncommand: {command}\n{}",
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
    let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
        stage: "disk image",
        error,
    })?;
    let (mut command, command_line) = qemu_command_with_disk(&kernel, disk.path());
    let status = command.status().map_err(|error| QemuError::Spawn {
        command: command_line.clone(),
        error: error.to_string(),
    })?;
    disk.remove();
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

    if kind == TestKind::PayloadArgs {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_args()?;
        let (command, command_line) = qemu_command_with_payload(&kernel, bundle.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        return verify_payload_args_result(
            &command_line,
            completed.status.code(),
            &completed.output,
        );
    }

    if kind == TestKind::PayloadStdin {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_stdin()?;
        let (command, command_line) = qemu_command_with_payload(&kernel, bundle.path());
        let completed = run_stdin_command(command, command_line.clone(), deadline)?;
        bundle.remove();
        return verify_payload_stdin_result(
            &command_line,
            completed.status.code(),
            &completed.output,
        );
    }

    if kind == TestKind::File {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::FileFd {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_fd()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_fd_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::FileWrite {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_write()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_write_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::FileUnlink {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_unlink()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_unlink_result(
            &command_line,
            completed.status.code(),
            &completed.output,
        );
    }

    if kind == TestKind::FileRename {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_rename()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_rename_result(
            &command_line,
            completed.status.code(),
            &completed.output,
        );
    }

    if kind == TestKind::FileMkdir {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_mkdir()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_mkdir_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::FileSpawn {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_spawn()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_spawn_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::FileWaitpid {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_waitpid()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_waitpid_result(
            &command_line,
            completed.status.code(),
            &completed.output,
        );
    }

    if kind == TestKind::FileStat {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_stat()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_stat_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::FileSeek {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_file_seek()?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) =
            qemu_command_with_payload_and_disk(&kernel, bundle.path(), disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        disk.remove();
        return verify_file_seek_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::Sched {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_sched()?;
        let (command, command_line) = qemu_command_with_payload(&kernel, bundle.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        bundle.remove();
        return verify_sched_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::SchedIo {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_sched_io()?;
        let (command, command_line) = qemu_command_with_payload(&kernel, bundle.path());
        let completed = run_sched_io_command(command, command_line.clone(), deadline)?;
        bundle.remove();
        return verify_sched_io_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::SchedIoPartial {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let bundle = PayloadBundle::create_sched_io()?;
        let (command, command_line) = qemu_command_with_payload(&kernel, bundle.path());
        let completed = run_sched_io_partial_command(command, command_line.clone(), deadline)?;
        bundle.remove();
        return verify_sched_io_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::Shell {
        let kernel = cargo::build_kernel(false).map_err(QemuError::Build)?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) = qemu_command_with_disk(&kernel, disk.path());
        let completed = run_shell_command(command, command_line.clone(), deadline)?;
        disk.remove();
        return verify_shell_result(&command_line, completed.status.code(), &completed.output);
    }

    if kind == TestKind::Virtio {
        let kernel = cargo::build_kernel_for_test(kind.feature()).map_err(QemuError::Build)?;
        let disk = crate::disk::DiskImage::create().map_err(|error| QemuError::Bundle {
            stage: "disk image",
            error,
        })?;
        let (command, command_line) = qemu_command_with_disk(&kernel, disk.path());
        let completed = run_command_with_capture(command, command_line.clone(), deadline)?;
        disk.remove();
        return verify_test_result(
            &command_line,
            kind,
            completed.status.code(),
            &completed.output,
        );
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

/// Ready frameを待ってStdin frame列を送り、終了まで出力を集める。
/// 入力の一括書き込み後にstdinを閉じるため、kernelはUART bufferから順に引く。
fn run_stdin_command(
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

    if let Err(failure) =
        wait_for_output(&mut child, &readers, PAYLOAD_READY_TEXT, started, deadline)
    {
        return finish_stdin_failure(child, readers, command_line, deadline, failure);
    }

    let write_result = child
        .stdin
        .take()
        .expect("stdin test stdin must be piped")
        .write_all(STDIN_TEST_FRAMES);
    if let Err(error) = write_result {
        let cleanup = terminate_and_reap(&mut child);
        let mut output = readers.join().unwrap_or_else(|join_error| join_error);
        if let Err(cleanup_error) = cleanup {
            output.push_str("\nQEMU cleanup error: ");
            output.push_str(&cleanup_error);
        }
        return Err(QemuError::Wait {
            command: command_line,
            error: format!("could not write stdin frames: {error}\n{output}"),
        });
    }

    collect_process_output(
        child,
        move || readers.join(),
        command_line,
        deadline,
        deadline.saturating_sub(started.elapsed()),
    )
}

/// sched-io検査の入力。`b3`を観測してから送る1 byteのStdin frame。
/// marker待ち後の送信なので、reader processは必ず一度blockする。
const SCHED_IO_STDIN_FRAME: &[u8] = b"MCF1\x07\0\0\0\x01\0\0\0z";

/// 分割送信の間にguestが前のchunkをconsumeするための待機時間。
/// scheduler loopは常時`stdin_pending`をpollするため、100 msあれば
/// 到着byteは必ずstagingへ吸い込まれる。
const SCHED_IO_PARTIAL_SETTLE: Duration = Duration::from_millis(100);

/// sched-io検査: Readyを待ち、quick processの`b3\n`が出力へ現れてから
/// Stdin frameを送り、終了まで出力を集める。readerがblock中に他processが
/// 進むことを`b3 < r2`の順序で検証するため、入力は必ずmarker観測後に送る。
fn run_sched_io_command(
    command: Command,
    command_line: String,
    deadline: Duration,
) -> Result<CompletedProcess, QemuError> {
    run_sched_io_script(
        command,
        command_line,
        deadline,
        &[(Some("b3\n"), SCHED_IO_STDIN_FRAME)],
    )
}

/// sched-io-partial検査: Stdin frameをheader途中と残りに分けて送る。
/// `b3`観測後に最初の5 byte（magic + kind、length未着）を送り、settleして
/// guestがpartial headerをconsume・再blockしたことを確実にしてから残りを送る。
/// 再開可能でないdecoderなら続きを先頭からdecodeしてdesync→fatalとなるため、
/// `r2`到達と正常終了がそのままresume経路の証明になる。
fn run_sched_io_partial_command(
    command: Command,
    command_line: String,
    deadline: Duration,
) -> Result<CompletedProcess, QemuError> {
    run_sched_io_script(
        command,
        command_line,
        deadline,
        &[
            (Some("b3\n"), &SCHED_IO_STDIN_FRAME[..5]),
            (Some("a1\n"), &SCHED_IO_STDIN_FRAME[5..]),
        ],
    )
}

/// `(marker, chunk)`列を順に送るsched-io系の実行。各chunkは対応するmarkerが
/// 出力へ現れてから書き込み、最後でなければ`SCHED_IO_PARTIAL_SETTLE`待って
/// guest側のconsumeを確実にする。
fn run_sched_io_script(
    mut command: Command,
    command_line: String,
    deadline: Duration,
    steps: &[(Option<&str>, &[u8])],
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

    if let Err(failure) =
        wait_for_output(&mut child, &readers, PAYLOAD_READY_TEXT, started, deadline)
    {
        return finish_stdin_failure(child, readers, command_line, deadline, failure);
    }

    let mut stdin = child
        .stdin
        .take()
        .expect("sched-io test stdin must be piped");
    for (index, (marker, chunk)) in steps.iter().enumerate() {
        // quick processの最終出力などのmarkerを待ってからstdinを送る。
        // この時点でreaderは必ずblock済みであり、spinはまだbusy-waitの途中である。
        if let Some(marker) = marker
            && let Err(failure) = wait_for_output(&mut child, &readers, marker, started, deadline)
        {
            return finish_stdin_failure(child, readers, command_line, deadline, failure);
        }
        if let Err(error) = stdin.write_all(chunk) {
            let cleanup = terminate_and_reap(&mut child);
            let mut output = readers.join().unwrap_or_else(|join_error| join_error);
            if let Err(cleanup_error) = cleanup {
                output.push_str("\nQEMU cleanup error: ");
                output.push_str(&cleanup_error);
            }
            return Err(QemuError::Wait {
                command: command_line,
                error: format!("could not write stdin frames: {error}\n{output}"),
            });
        }
        if index + 1 < steps.len() {
            std::thread::sleep(SCHED_IO_PARTIAL_SETTLE);
        }
    }

    collect_process_output(
        child,
        move || readers.join(),
        command_line,
        deadline,
        deadline.saturating_sub(started.elapsed()),
    )
}

fn finish_stdin_failure(
    mut child: Child,
    readers: LiveOutputReaders,
    command: String,
    deadline: Duration,
    failure: ShellFailure,
) -> Result<CompletedProcess, QemuError> {
    let cleanup = terminate_and_reap(&mut child);
    let mut output = readers.join().unwrap_or_else(|error| error);
    if let Err(cleanup_error) = cleanup {
        output.push_str("\nQEMU cleanup error: ");
        output.push_str(&cleanup_error);
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
        ShellFailure::Exited => Err(QemuError::MissingControlFrame {
            command,
            expected: "READY",
            output,
        }),
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
    collect_process_output(
        child,
        move || readers.join(),
        command_line,
        deadline,
        deadline,
    )
}

/// `wait_until_exit`の共通末尾: 正常終了ならreader出力を回収し、timeout/poll
/// 失敗なら子processを止めて回収済み出力をerrorへ添える。
/// `join`はreader種別（`OutputReaders`/`LiveOutputReaders`）を吸収する
/// 一回限りのclosureである。
fn collect_process_output(
    mut child: Child,
    join: impl FnOnce() -> Result<String, String>,
    command_line: String,
    deadline: Duration,
    remaining: Duration,
) -> Result<CompletedProcess, QemuError> {
    match wait_until_exit(&mut child, remaining) {
        Ok(status) => {
            let output = join().map_err(|error| QemuError::Wait {
                command: command_line.clone(),
                error,
            })?;
            Ok(CompletedProcess { status, output })
        }
        Err(WaitFailure::TimedOut) => {
            let cleanup = terminate_and_reap(&mut child);
            let mut output = join().unwrap_or_else(|error| error);
            if let Err(cleanup_error) = cleanup {
                output.push_str("\nQEMU cleanup error: ");
                output.push_str(&cleanup_error);
            }
            Err(QemuError::TimedOut {
                command: command_line,
                deadline,
                output,
            })
        }
        Err(WaitFailure::Poll(error)) => {
            let cleanup = terminate_and_reap(&mut child);
            let output = join().unwrap_or_else(|error| error);
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
        Ok(crate::cargo::combine_output(&stdout, &stderr))
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

/// payload検査で期待されるcontrol frame列 (Ready→spawned→stdout→stderr→Exit→cleanup)。
const PAYLOAD_EXPECTED_FRAMES: [&[u8]; 6] = [
    PAYLOAD_READY_FRAME,
    PAYLOAD_SPAWNED_HELLO_FRAME,
    PAYLOAD_STDOUT_FRAME,
    PAYLOAD_STDERR_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// payload-args検査で期待されるcontrol frame列 (Ready→spawned→argv echo×3→Exit→cleanup)。
const PAYLOAD_ARGS_EXPECTED_FRAMES: [&[u8]; 7] = [
    PAYLOAD_READY_FRAME,
    PAYLOAD_SPAWNED_HELLO_FRAME,
    ARGS_STDOUT_HELLO_FRAME,
    ARGS_STDOUT_ALPHA_FRAME,
    ARGS_STDOUT_BRAVO_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// payload-stdin検査で期待されるcontrol frame列 (Ready→spawned→echo→echo→Exit→cleanup)。
const PAYLOAD_STDIN_EXPECTED_FRAMES: [&[u8]; 6] = [
    PAYLOAD_READY_FRAME,
    PAYLOAD_SPAWNED_STDIN_CAT_FRAME,
    STDIN_STDOUT_AB_FRAME,
    STDIN_STDOUT_CDEF_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file検査で期待されるcontrol frame列 (Ready→spawned→file内容→Exit→cleanup)。
const FILE_EXPECTED_FRAMES: [&[u8]; 5] = [
    PAYLOAD_READY_FRAME,
    FILE_SPAWNED_FRAME,
    FILE_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file-fd検査で期待されるcontrol frame列。guestがopen/分割read/closeと
/// errno経路を通してから同じ内容を出力する。
const FILE_FD_EXPECTED_FRAMES: [&[u8]; 5] = [
    PAYLOAD_READY_FRAME,
    FILE_FD_SPAWNED_FRAME,
    FILE_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file-write検査で期待されるcontrol frame列。guestがcreate/write/close/
/// 再open/再readの経路を通してから、書いた内容をstdoutへ出力する。
const FILE_WRITE_EXPECTED_FRAMES: [&[u8]; 5] = [
    PAYLOAD_READY_FRAME,
    FILE_WRITE_SPAWNED_FRAME,
    FILE_WRITE_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file-unlink検査で期待されるcontrol frame列。guestがcreate/write/
/// unlink/fd失効/再作成の経路を通してから、読み戻した内容をstdoutへ
/// 出力する。
const FILE_UNLINK_EXPECTED_FRAMES: [&[u8]; 5] = [
    PAYLOAD_READY_FRAME,
    FILE_UNLINK_SPAWNED_FRAME,
    FILE_UNLINK_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file-seek検査で期待されるcontrol frame列。guestがpread/pwrite/lseekの
/// 位置指定I/O経路を通してから、検証済みの旨をstdoutへ出力する。
const FILE_SEEK_EXPECTED_FRAMES: [&[u8]; 5] = [
    PAYLOAD_READY_FRAME,
    FILE_SEEK_SPAWNED_FRAME,
    FILE_SEEK_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file-rename検査で期待されるcontrol frame列。guestがrename/fd継続/
/// 置き換え/errnoの経路を通してから、検証済みの旨をstdoutへ出力する。
const FILE_RENAME_EXPECTED_FRAMES: [&[u8]; 5] = [
    PAYLOAD_READY_FRAME,
    FILE_RENAME_SPAWNED_FRAME,
    FILE_RENAME_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file-mkdir検査で期待されるcontrol frame列。guestがmkdir/中身file/
/// ENOTEMPTY/nested/削除の経路を通してから、検証済みの旨をstdoutへ出力する。
const FILE_MKDIR_EXPECTED_FRAMES: [&[u8]; 5] = [
    PAYLOAD_READY_FRAME,
    FILE_MKDIR_SPAWNED_FRAME,
    FILE_MKDIR_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file-waitpid検査で期待されるcontrol frame列。parentがwaitpidで
/// blockするため、childのstdoutとExitはparentの`waitpid verified`と
/// Exitより必ず先に出る＝順序が確定的でexact照合がblockingの直接証拠
/// になる。
const FILE_WAITPID_EXPECTED_FRAMES: [&[u8]; 7] = [
    PAYLOAD_READY_FRAME,
    FILE_WAITPID_SPAWNED_FRAME,
    FILE_WAITPID_CHILD_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    FILE_WAITPID_PARENT_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

/// file-stat検査で期待されるcontrol frame列。guestがstat/fstatの
/// metadata・errno・EFAULTの経路を通してから、検証済みの旨をstdoutへ
/// 出力する。
const FILE_STAT_EXPECTED_FRAMES: [&[u8]; 5] = [
    PAYLOAD_READY_FRAME,
    FILE_STAT_SPAWNED_FRAME,
    FILE_STAT_STDOUT_FRAME,
    PAYLOAD_EXIT_FRAME,
    PAYLOAD_DIAGNOSTIC_FRAME,
];

fn verify_payload_stdin_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &PAYLOAD_STDIN_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_file_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_file_fd_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_FD_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_file_write_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_WRITE_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_file_unlink_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_UNLINK_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_file_seek_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_SEEK_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_file_rename_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_RENAME_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_file_mkdir_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_MKDIR_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

/// file-spawn検証: manifestのpid 0がspawn syscallで`DOCS/CHILD.ELF`を
/// 起動し、parentとchildの両方がstdout markerとExit(42) frameを出す
/// ことを確認する。childは親の残りの実行とどちらが先でもよいため、
/// frame順ではなく集合と内容を照合する。
fn verify_file_spawn_result(
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
    let Some(frames) = collect_payload_frames(output.as_bytes()) else {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    };

    let mut stdout = Vec::new();
    let mut exits = Vec::new();
    let mut spawned = 0usize;
    let mut last_diagnostic = Vec::new();
    for (kind, payload) in &frames {
        match kind {
            FrameKind::Stdout => stdout.extend_from_slice(payload),
            FrameKind::Exit => {
                let Ok(code) = <[u8; 4]>::try_from(*payload) else {
                    return Err(QemuError::PayloadFrames {
                        command: command.to_owned(),
                        output: output.to_owned(),
                    });
                };
                exits.push(u32::from_le_bytes(code));
            }
            FrameKind::Diagnostic => {
                if **payload == FILE_SPAWN_SPAWNED_FRAME[FRAME_HEADER_LEN..] {
                    spawned += 1;
                }
                last_diagnostic = payload.to_vec();
            }
            _ => {}
        }
    }

    let stdout_ok = stdout
        .windows(FILE_SPAWN_PARENT_STDOUT.len())
        .any(|window| window == FILE_SPAWN_PARENT_STDOUT)
        && stdout
            .windows(FILE_SPAWN_CHILD_STDOUT.len())
            .any(|window| window == FILE_SPAWN_CHILD_STDOUT);
    if !stdout_ok || exits != [42, 42] || spawned != 1 || last_diagnostic.is_empty() {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

/// file-waitpid検証: parentがwaitpidでblockし、childの`spawn-child`と
/// Exit(42)が先に出てから、parentがreapしたcode 42を確認して
/// `waitpid verified`とExit(42)を出す。順序が確定的なのでexact照合する。
fn verify_file_waitpid_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_WAITPID_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

/// file-stat検証: guestがstat/fstatの契約を全て確認し、`stat verified`と
/// Exit(42)を出す。単一processなのでexact照合できる。
fn verify_file_stat_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &FILE_STAT_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

/// Ready以降のcontrol frameを`(kind, payload)`列として集める。Readyがない、
/// frame列が途中で切れる、またはdecode不能な場合は`None`を返す。
fn collect_payload_frames(output: &[u8]) -> Option<Vec<(FrameKind, &[u8])>> {
    let start = output
        .windows(PAYLOAD_READY_FRAME.len())
        .position(|window| window == PAYLOAD_READY_FRAME)?;
    let mut frames = Vec::new();
    let mut remaining = &output[start..];
    while !remaining.is_empty() {
        let header = FrameHeader::decode(remaining.get(..FRAME_HEADER_LEN)?).ok()?;
        let payload_len = usize::try_from(header.payload_len).ok()?;
        let payload = remaining.get(FRAME_HEADER_LEN..FRAME_HEADER_LEN + payload_len)?;
        frames.push((header.kind, payload));
        remaining = &remaining[FRAME_HEADER_LEN + payload_len..];
    }
    Some(frames)
}

/// sched検証: 2 processのstdout markerが交差すること、両方のProcExit frameが
/// 届くこと、kernelが切り替え回数を報告することを確認する。
///
/// `a1` < `b1` < `a3` の順序は、busy-wait中のprocess Aの生存期間内に
/// process Bが走ったこと＝timerプリエンプションの直接証拠である。
/// 逐次実行なら必ず `a*…b*` か `b*…a*` の単調列になるため、この条件は
/// 順次実行を確実に弾く。
fn verify_sched_result(
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
    let Some(frames) = collect_payload_frames(output.as_bytes()) else {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    };

    let mut stdout = Vec::new();
    let mut proc_exits = Vec::new();
    let mut last_diagnostic = String::new();
    for (kind, payload) in &frames {
        match kind {
            FrameKind::Stdout => stdout.extend_from_slice(payload),
            FrameKind::ProcExit => {
                let Ok(decoded) = ProcExitPayload::decode(payload) else {
                    return Err(QemuError::PayloadFrames {
                        command: command.to_owned(),
                        output: output.to_owned(),
                    });
                };
                proc_exits.push((decoded.pid, decoded.code));
            }
            FrameKind::Diagnostic => {
                last_diagnostic = String::from_utf8_lossy(payload).into_owned();
            }
            _ => {}
        }
    }

    let position = |marker: &[u8]| {
        stdout
            .windows(marker.len())
            .position(|window| window == marker)
    };
    let (a1, a3, b1) = (position(b"a1\n"), position(b"a3\n"), position(b"b1\n"));
    let interleaved = matches!((a1, a3, b1), (Some(a1), Some(a3), Some(b1)) if a1 < b1 && b1 < a3);
    // BがAのspin中に終了するので、ProcExitはpid 1→0の順で確定的である。
    let exits_ok = proc_exits == [(1, 7), (0, 0)];
    let switches_ok = last_diagnostic
        .strip_prefix("\r\nMiniOS payload: ok processes=2 switches=")
        .and_then(|text| text.trim().parse::<usize>().ok())
        .is_some_and(|switches| switches >= 1);
    if !interleaved || !exits_ok || !switches_ok {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

/// sched-io検証: stdin待ちreaderの`r1`と`r2`の間に、busy-waitするprocessと
/// 短命processの出力がすべて挟まること、3つとも`ProcExit` frameを出すこと、
/// kernelが`processes=3`と切り替え回数を報告することを確認する。
///
/// 決定的な条件は`b3 < r2`である。hostは`b3`を観測してからstdinを送るため、
/// `r2`は必ず`b3`の後に出る。`read`がkernel内で停まる旧来の実装なら
/// `b1`すら到着せずtimeoutになるため、この条件は「block中も他processが
/// 進む」ことの直接証拠になる。
fn verify_sched_io_result(
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
    let Some(frames) = collect_payload_frames(output.as_bytes()) else {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    };

    let mut stdout = Vec::new();
    let mut proc_exits = Vec::new();
    let mut last_diagnostic = String::new();
    for (kind, payload) in &frames {
        match kind {
            FrameKind::Stdout => stdout.extend_from_slice(payload),
            FrameKind::ProcExit => {
                let Ok(decoded) = ProcExitPayload::decode(payload) else {
                    return Err(QemuError::PayloadFrames {
                        command: command.to_owned(),
                        output: output.to_owned(),
                    });
                };
                proc_exits.push((decoded.pid, decoded.code));
            }
            FrameKind::Diagnostic => {
                last_diagnostic = String::from_utf8_lossy(payload).into_owned();
            }
            _ => {}
        }
    }

    let position = |marker: &[u8]| {
        stdout
            .windows(marker.len())
            .position(|window| window == marker)
    };
    let (r1, r2, a1, a3, b1, b3) = (
        position(b"r1\n"),
        position(b"r2\n"),
        position(b"a1\n"),
        position(b"a3\n"),
        position(b"b1\n"),
        position(b"b3\n"),
    );
    let ordered = matches!(
        (r1, r2, a1, a3, b1, b3),
        (Some(r1), Some(r2), Some(a1), Some(a3), Some(b1), Some(b3))
            if r1 < r2 && a1 < a3 && b1 < b3 && b3 < r2
    );
    // exit順はschedule次第で揺れるため、pid/codeの集合で検査する。
    let mut exits = proc_exits.clone();
    exits.sort_unstable();
    let exits_ok = exits == [(0, 5), (1, 0), (2, 7)];
    let switches_ok = last_diagnostic
        .strip_prefix("\r\nMiniOS payload: ok processes=3 switches=")
        .and_then(|text| text.trim().parse::<usize>().ok())
        .is_some_and(|switches| switches >= 1);
    if !ordered || !exits_ok || !switches_ok {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

fn verify_payload_args_result(
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
    if !has_exact_payload_frames(output.as_bytes(), &PAYLOAD_ARGS_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
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
    if !has_exact_payload_frames(output_bytes, &PAYLOAD_EXPECTED_FRAMES) {
        return Err(QemuError::PayloadFrames {
            command: command.to_owned(),
            output: output.to_owned(),
        });
    }
    Ok(output.to_owned())
}

/// Ready前のfirmware出力を許可し、Readyから出力末尾までをpayload control frameとして
/// 完全に消費する。
fn has_exact_payload_frames(output: &[u8], expected_frames: &[&[u8]]) -> bool {
    let Some(start) = output
        .windows(PAYLOAD_READY_FRAME.len())
        .position(|window| window == PAYLOAD_READY_FRAME)
    else {
        return false;
    };
    let mut remaining = &output[start..];
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
        Self::create_with(payload_bundle_bytes()?)
    }

    fn create_args() -> Result<Self, QemuError> {
        let elf = built_guest_elf_bytes()?;
        Self::create_with(payload_args_bundle_bytes(&elf)?)
    }

    fn create_stdin() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_STDIN_CAT)?;
        Self::create_with(payload_stdin_bundle_bytes(&elf)?)
    }

    fn create_file() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_READ)?;
        Self::create_with(payload_file_bundle_bytes(&elf)?)
    }

    fn create_file_fd() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_FD)?;
        Self::create_with(payload_file_fd_bundle_bytes(&elf)?)
    }

    fn create_file_write() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_WRITE)?;
        Self::create_with(payload_file_write_bundle_bytes(&elf)?)
    }

    fn create_file_unlink() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_UNLINK)?;
        Self::create_with(payload_file_unlink_bundle_bytes(&elf)?)
    }

    fn create_file_seek() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_SEEK)?;
        Self::create_with(payload_file_seek_bundle_bytes(&elf)?)
    }

    fn create_file_rename() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_RENAME)?;
        Self::create_with(payload_file_rename_bundle_bytes(&elf)?)
    }

    fn create_file_mkdir() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_MKDIR)?;
        Self::create_with(payload_file_mkdir_bundle_bytes(&elf)?)
    }

    fn create_file_spawn() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_SPAWN)?;
        Self::create_with(payload_file_spawn_bundle_bytes(&elf)?)
    }

    fn create_file_waitpid() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_WAITPID)?;
        Self::create_with(payload_file_waitpid_bundle_bytes(&elf)?)
    }

    fn create_file_stat() -> Result<Self, QemuError> {
        let elf = built_bin_elf_bytes(crate::guest::GUEST_FILE_STAT)?;
        Self::create_with(payload_file_stat_bundle_bytes(&elf)?)
    }

    fn create_sched() -> Result<Self, QemuError> {
        let spin = built_bin_elf_bytes(crate::guest::GUEST_SCHED_A)?;
        let quick = built_bin_elf_bytes(crate::guest::GUEST_SCHED_B)?;
        Self::create_with(sched_bundle_bytes(&spin, &quick)?)
    }

    fn create_sched_io() -> Result<Self, QemuError> {
        let reader = built_bin_elf_bytes(crate::guest::GUEST_SCHED_R)?;
        let spin = built_bin_elf_bytes(crate::guest::GUEST_SCHED_A)?;
        let quick = built_bin_elf_bytes(crate::guest::GUEST_SCHED_B)?;
        Self::create_with(sched_io_bundle_bytes(&reader, &spin, &quick)?)
    }

    fn create_with(bytes: Vec<u8>) -> Result<Self, QemuError> {
        let path = std::env::temp_dir().join(format!(
            "minios-payload-{}-{}.mcb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock must be after Unix epoch")
                .as_nanos()
        ));
        std::fs::write(&path, bytes).map_err(|error| QemuError::Spawn {
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
/// stderr frame経路を担い、正規builderで組み立てる。
fn payload_bundle_bytes() -> Result<Vec<u8>, QemuError> {
    assemble_test_bundle(b"version=1\nname=hello\n", &payload_elf_bytes())
}

/// QEMU検査用bundleを正規builderで組み立てる。手書きのheader組立は持たない。
fn assemble_test_bundle(manifest: &[u8], elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    crate::bundle::build_bundle(manifest, elf)
        .map(|bundle| bundle.bytes().to_vec())
        .map_err(|error| QemuError::Bundle {
            stage: "bundle layout",
            error: error.to_string(),
        })
}

/// build済みRust guestのELF bytesを読み込む。
/// QEMU実行path専用であり、unit testは共有の`guest_bytes`を使う。
/// 並行testが別々にcargoを起動すると成果物の再linkと読み取りが競合するため、
/// test process内のcargo起動は`guest_bytes`の一度だけに絞る。
fn built_guest_elf_bytes() -> Result<Vec<u8>, QemuError> {
    let elf_path = crate::guest::build_guest().map_err(|error| QemuError::Bundle {
        stage: "guest build",
        error: error.to_string(),
    })?;
    std::fs::read(&elf_path).map_err(|error| QemuError::Bundle {
        stage: "guest ELF read",
        error: format!("{}: {error}", elf_path.display()),
    })
}

/// 指定したguest binをbuildしてELF bytesを読み込む。
fn built_bin_elf_bytes(name: &str) -> Result<Vec<u8>, QemuError> {
    let elf_path = crate::guest::build_guest_bin(name).map_err(|error| QemuError::Bundle {
        stage: "guest build",
        error: error.to_string(),
    })?;
    std::fs::read(&elf_path).map_err(|error| QemuError::Bundle {
        stage: "guest ELF read",
        error: format!("{}: {error}", elf_path.display()),
    })
}

/// payload-args検査用bundle: Rust guestのELFと`arg=`付きmanifestを
/// 正規のMiniBundleへ組み立てる。guestはargvを順にstdoutへ出してexit(42)する。
/// 手書きのargv loop ELFはRust guestと役割が重複するため、この経路では使わない。
fn payload_args_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=hello\narg=alpha\narg=bravo\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// sched検証用bundle: busy-waitするguestとすぐ終わるguestの2 imageを
/// manifest v2で組み立てる。process indexは宣言順 (spin=0, quick=1)。
fn sched_bundle_bytes(spin: &[u8], quick: &[u8]) -> Result<Vec<u8>, QemuError> {
    let images = [
        crate::bundle::BundleImage {
            name: "spin",
            args: &[],
            elf: spin,
        },
        crate::bundle::BundleImage {
            name: "quick",
            args: &[],
            elf: quick,
        },
    ];
    crate::bundle::build_multi_bundle(&images)
        .map(|bundle| bundle.bytes().to_vec())
        .map_err(|error| QemuError::Bundle {
            stage: "sched bundle layout",
            error: error.to_string(),
        })
}

/// sched-io検証用bundle: stdinで待つguest・busy-waitするguest・すぐ終わる
/// guestの3 imageをmanifest v2で組み立てる。process indexは宣言順
/// (reader=0, spin=1, quick=2)。
fn sched_io_bundle_bytes(reader: &[u8], spin: &[u8], quick: &[u8]) -> Result<Vec<u8>, QemuError> {
    let images = [
        crate::bundle::BundleImage {
            name: "reader",
            args: &[],
            elf: reader,
        },
        crate::bundle::BundleImage {
            name: "spin",
            args: &[],
            elf: spin,
        },
        crate::bundle::BundleImage {
            name: "quick",
            args: &[],
            elf: quick,
        },
    ];
    crate::bundle::build_multi_bundle(&images)
        .map(|bundle| bundle.bytes().to_vec())
        .map_err(|error| QemuError::Bundle {
            stage: "sched-io bundle layout",
            error: error.to_string(),
        })
}

/// payload-stdin検査用bundle: cat guestのELFと引数なしmanifestを
/// 正規のMiniBundleへ組み立てる。guestはstdinをechoしてexit(42)する。
fn payload_stdin_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=stdin-cat\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file検査用bundle: file_read guestのELFと引数なしmanifestを
/// 正規のMiniBundleへ組み立てる。guestは`DOCS/NOTE.TXT`を読んで
/// stdoutへ書きexit(42)する。
fn payload_file_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-read\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-fd検査用bundle: file_fd guestのELFと引数なしmanifestを組み立てる。
/// guestはopen/分割read/closeを確かめて内容をstdoutへ書きexit(42)する。
fn payload_file_fd_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-fd\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-write検査用bundle: file_write guestのELFと引数なしmanifestを
/// 組み立てる。guestはcreate/write/close/再openで内容を往復させてから
/// stdoutへ書きexit(42)する。
fn payload_file_write_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-write\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-unlink検査用bundle: file_unlink guestのELFと引数なしmanifestを
/// 組み立てる。guestはcreate/write/unlink/fd失効/再作成の経路を確かめて
/// 内容をstdoutへ書きexit(42)する。
fn payload_file_unlink_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-unlink\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-seek検査用bundle: file_seek guestのELFと引数なしmanifestを
/// 組み立てる。guestはpread/pwrite/lseekの位置指定I/Oを確かめて
/// `seek verified`をstdoutへ書きexit(42)する。
fn payload_file_seek_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-seek\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-rename検査用bundle: file_rename guestのELFと引数なしmanifestを
/// 組み立てる。guestはrename/fd継続/置き換え/errnoの経路を確かめて
/// `rename verified`をstdoutへ書きexit(42)する。
fn payload_file_rename_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-rename\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-mkdir検査用bundle: file_mkdir guestのELFと引数なしmanifestを
/// 組み立てる。guestはmkdir/中身file/ENOTEMPTY/nested/削除の経路を確かめて
/// `mkdir verified`をstdoutへ書きexit(42)する。
fn payload_file_mkdir_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-mkdir\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-spawn検査用bundle: file_spawn guestのELFと引数なしmanifestを
/// 組み立てる。guestはgetpid/spawnとerrno経路を確かめて`spawn verified`
/// をstdoutへ書きexit(42)し、spawnされた`DOCS/CHILD.ELF`のprocessも
/// `spawn-child`を書いてexit(42)する。
fn payload_file_spawn_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-spawn\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-waitpid検査用bundle: file_waitpid guestのELFと引数なしmanifestを
/// 組み立てる。guestは`DOCS/CHILD.ELF`をspawnし、waitpidでblockされて
/// childの終了code 42を回収し、errno経路を確かめて`waitpid verified`
/// をstdoutへ書きexit(42)する。
fn payload_file_waitpid_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-waitpid\n";
    assemble_test_bundle(MANIFEST, elf)
}

/// file-stat検査用bundle: file_stat guestのELFと引数なしmanifestを
/// 組み立てる。guestはstat/fstatのfile・directory・errno・EFAULT経路を
/// 確かめて`stat verified`をstdoutへ書きexit(42)する。
fn payload_file_stat_bundle_bytes(elf: &[u8]) -> Result<Vec<u8>, QemuError> {
    const MANIFEST: &[u8] = b"version=1\nname=file-stat\n";
    assemble_test_bundle(MANIFEST, elf)
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

fn qemu_command_with_disk(kernel: &Path, disk: &Path) -> (Command, String) {
    let mut args = qemu_args(kernel);
    args.push("-drive".to_owned());
    args.push(format!(
        "file={},format=raw,if=none,id=blk0",
        disk.display()
    ));
    args.push("-global".to_owned());
    args.push("virtio-mmio.force-legacy=false".to_owned());
    args.push("-device".to_owned());
    args.push("virtio-blk-device,drive=blk0,bus=virtio-mmio-bus.0".to_owned());
    let command_line = render_command(QEMU_PROGRAM, &args);
    let mut command = Command::new(QEMU_PROGRAM);
    command.args(&args);
    (command, command_line)
}

fn qemu_command_with_payload_and_disk(
    kernel: &Path,
    bundle: &Path,
    disk: &Path,
) -> (Command, String) {
    let mut args = qemu_args(kernel);
    args.push("-device".to_owned());
    args.push(format!(
        "loader,file={},addr=0x87800000,force-raw=on",
        bundle.display()
    ));
    args.push("-drive".to_owned());
    args.push(format!(
        "file={},format=raw,if=none,id=blk0",
        disk.display()
    ));
    args.push("-global".to_owned());
    args.push("virtio-mmio.force-legacy=false".to_owned());
    args.push("-device".to_owned());
    args.push("virtio-blk-device,drive=blk0,bus=virtio-mmio-bus.0".to_owned());
    let command_line = render_command(QEMU_PROGRAM, &args);
    let mut command = Command::new(QEMU_PROGRAM);
    command.args(&args);
    (command, command_line)
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
    if kind == TestKind::UserTrap
        && !normalized
            .lines()
            .any(|line| line == USER_TRAP_FAULT_DIAGNOSTIC)
    {
        return Err(QemuError::MissingMarker {
            command: command.to_owned(),
            expected: USER_TRAP_FAULT_DIAGNOSTIC,
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
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "ls        List a directory"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "cat       Read a file"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "rm        Remove a file"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "mkdir     Create a directory"))
        .and_then(|()| {
            expect_shell_line(
                transcript,
                &mut cursor,
                "rmdir     Remove an empty directory",
            )
        })
        .and_then(|()| {
            expect_shell_line(
                transcript,
                &mut cursor,
                "mv        Rename a file or directory",
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
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> ls"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "        18 HELLO.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "<DIR> DOCS"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "        19 Long File Name.txt"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> ls DOCS"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "        17 NOTE.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "       204 CHILD.ELF"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> cat DOCS/NOTE.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "note inside docs"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> cat Long File Name.txt"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "long file contents"))
        // `rm`後のlsはLFN fileを列挙しないこと（逐行照合がその行を飛ばさない）
        // と、catがnot foundを返すことを確認する。
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> rm Long File Name.txt"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> ls"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "        18 HELLO.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "<DIR> DOCS"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> cat Long File Name.txt"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "virtio: file not found"))
        // `mkdir`後のlsは新しいdirectoryを列挙し、非空dirへの`rmdir`は
        // エラー、`rmdir`後のlsはそのdirectoryを列挙しないことを確認する。
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> mkdir NEWDIR"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> ls"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "        18 HELLO.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "<DIR> DOCS"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "<DIR> NEWDIR"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> rmdir DOCS"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "virtio: directory not empty"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> rmdir NEWDIR"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> ls"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "        18 HELLO.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "<DIR> DOCS"))
        // `mv`はfileとdirの両方を改名する。file改名後のlsは新名を列挙し、
        // dir改名後は新名のまま中身へ到達できる（`..`は親clusterを指す
        // ため更新不要）。
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> mv HELLO.TXT WORLD.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> ls"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "        18 WORLD.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "<DIR> DOCS"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> mv WORLD.TXT HELLO.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> mv DOCS NOTESD"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> ls"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "        18 HELLO.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "<DIR> NOTESD"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> cat NOTESD/NOTE.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "note inside docs"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> mv NOTESD DOCS"))
        // cross-directory move：fileをdirの中へ移し、新pathで読み、
        // rootへ戻す。`ls`は移動後のroot列挙でHELLO.TXTが消える。
        .and_then(|()| {
            expect_shell_line(
                transcript,
                &mut cursor,
                "minios> mv HELLO.TXT DOCS/MOVED.TXT",
            )
        })
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> ls"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "<DIR> DOCS"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "minios> cat DOCS/MOVED.TXT"))
        .and_then(|()| expect_shell_line(transcript, &mut cursor, "hello from virtio"))
        .and_then(|()| {
            expect_shell_line(
                transcript,
                &mut cursor,
                "minios> mv DOCS/MOVED.TXT HELLO.TXT",
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
            TestKind::Sched,
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
        let supervisor_page_rejected = concat!(
            "OpenSBI\r\n",
            "[MINIOS_TEST] user-trap: rejected\r\n",
            "MiniOS user trap: scause=0x000000000000000f ",
            "stval=0x0000000010000000\r\n",
        );

        assert!(verify_test_result(TEST_COMMAND, TestKind::UserTrap, Some(0), rejected).is_err());
        assert_eq!(
            verify_test_result(
                TEST_COMMAND,
                TestKind::UserTrap,
                Some(0),
                supervisor_page_rejected
            ),
            Ok(supervisor_page_rejected.to_owned())
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
            "ls        List a directory",
            "cat       Read a file",
            "rm        Remove a file",
            "mkdir     Create a directory",
            "rmdir     Remove an empty directory",
            "mv        Rename a file or directory",
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

    #[test]
    fn bundle_error_reports_the_stage_and_cause() {
        let error = QemuError::Bundle {
            stage: "guest build",
            error: "guest build failed with status 101".to_owned(),
        };

        assert_eq!(
            error.to_string(),
            "could not build the QEMU test bundle (guest build):\nguest build failed with status 101"
        );
    }

    fn complete_shell_output() -> String {
        [
            "minios> help",
            "help      Show available commands",
            "info      Show system information",
            "uptime    Show elapsed time",
            "memory    Show physical memory statistics",
            "ls        List a directory",
            "cat       Read a file",
            "rm        Remove a file",
            "mkdir     Create a directory",
            "rmdir     Remove an empty directory",
            "mv        Rename a file or directory",
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
            "minios> ls",
            "        18 HELLO.TXT",
            "<DIR> DOCS",
            "        19 Long File Name.txt",
            "minios> ls DOCS",
            "        17 NOTE.TXT",
            "       204 CHILD.ELF",
            "minios> cat DOCS/NOTE.TXT",
            "note inside docs",
            "minios> cat Long File Name.txt",
            "long file contents",
            "minios> rm Long File Name.txt",
            "minios> ls",
            "        18 HELLO.TXT",
            "<DIR> DOCS",
            "minios> cat Long File Name.txt",
            "virtio: file not found",
            "minios> mkdir NEWDIR",
            "minios> ls",
            "        18 HELLO.TXT",
            "<DIR> DOCS",
            "<DIR> NEWDIR",
            "minios> rmdir DOCS",
            "virtio: directory not empty",
            "minios> rmdir NEWDIR",
            "minios> ls",
            "        18 HELLO.TXT",
            "<DIR> DOCS",
            "minios> mv HELLO.TXT WORLD.TXT",
            "minios> ls",
            "        18 WORLD.TXT",
            "<DIR> DOCS",
            "minios> mv WORLD.TXT HELLO.TXT",
            "minios> mv DOCS NOTESD",
            "minios> ls",
            "        18 HELLO.TXT",
            "<DIR> NOTESD",
            "minios> cat NOTESD/NOTE.TXT",
            "note inside docs",
            "minios> mv NOTESD DOCS",
            "minios> mv HELLO.TXT DOCS/MOVED.TXT",
            "minios> ls",
            "<DIR> DOCS",
            "minios> cat DOCS/MOVED.TXT",
            "hello from virtio",
            "minios> mv DOCS/MOVED.TXT HELLO.TXT",
            "minios> not-a-command",
            "unknown command: not-a-command; try 'help'",
            "minios> shutdown",
            "shutting down",
        ]
        .join("\n")
    }

    // Catches a bundle whose header, manifest, padding, ELF placement, or
    // digest drifts from the canonical layout the kernel parser validates.
    #[test]
    fn payload_bundle_is_canonical_and_self_consistent() {
        let bundle = payload_bundle_bytes().expect("fixture bundle must build");
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
        assert_eq!(&bundle[56..88], &crate::bundle::sha256(&digest_input)[..]);
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

    // Catches an args bundle that stops embedding the built Rust guest, or
    // drifts from the canonical manifest layout the kernel argv collector
    // parses (name first, then arg= lines).
    #[test]
    fn payload_args_bundle_carries_the_built_guest_and_manifest_arguments() {
        let bundle = payload_args_bundle_bytes(crate::guest::guest_bytes())
            .expect("guest bundle must build");
        let manifest_len = u64::from_le_bytes(bundle[32..40].try_into().unwrap()) as usize;
        assert_eq!(
            &bundle[96..96 + manifest_len],
            b"version=1\nname=hello\narg=alpha\narg=bravo\n"
        );
        // ELF payloadはbuild済みRust guestそのものである。
        let elf_offset = u64::from_le_bytes(bundle[40..48].try_into().unwrap()) as usize;
        assert_eq!(&bundle[elf_offset..], crate::guest::guest_bytes());
    }

    // Catches an args run that drops an argument frame, reorders them, or
    // emits anything besides the exact Ready/argv/Exit/cleanup sequence.
    #[test]
    fn payload_args_verification_requires_each_argument_frame_in_order() {
        let mut output = "OpenSBI\n[ok] traps\n".to_owned();
        for frame in PAYLOAD_ARGS_EXPECTED_FRAMES {
            output.push_str(&String::from_utf8_lossy(frame));
        }
        assert!(verify_payload_args_result(TEST_COMMAND, Some(0), &output).is_ok());

        let mut reordered = "boot\n".to_owned();
        for index in [0usize, 2, 1, 3, 4, 5] {
            reordered.push_str(&String::from_utf8_lossy(
                PAYLOAD_ARGS_EXPECTED_FRAMES[index],
            ));
        }
        assert!(matches!(
            verify_payload_args_result(TEST_COMMAND, Some(0), &reordered),
            Err(QemuError::PayloadFrames { .. })
        ));
        assert!(matches!(
            verify_payload_args_result(TEST_COMMAND, Some(1), &output),
            Err(QemuError::Failed { .. })
        ));
    }

    // payload-argsにはstderr frameがないため、二つのpayload検査で共有する
    // 診断がstderrを必須と誤記しないことを確かめる。
    #[test]
    fn payload_frame_error_describes_both_payload_test_variants() {
        let error = QemuError::PayloadFrames {
            command: TEST_COMMAND.to_owned(),
            output: "boot output".to_owned(),
        };
        let message = error.to_string();

        assert!(message.contains("expected control-frame sequence"));
        assert!(!message.contains("stdout/stderr"));
    }

    // Catches a payload run that misses any of the five control frames or
    // reorders them.
    #[test]
    fn payload_verification_requires_the_exact_frame_sequence() {
        let mut output = "OpenSBI\n[ok] traps\n".to_owned();
        for frame in PAYLOAD_EXPECTED_FRAMES {
            output.push_str(&String::from_utf8_lossy(frame));
        }
        assert_eq!(
            verify_payload_result(TEST_COMMAND, Some(0), &output).map(|_| ()),
            Ok(())
        );

        let truncated = {
            let expected = PAYLOAD_EXPECTED_FRAMES.concat();
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

    // Catches a sched run whose two processes executed strictly one after the
    // other (no preemption evidence), or that never reported a timer switch.
    #[test]
    fn sched_verification_requires_interleaved_output_and_switches() {
        let frame = |kind: u8, payload: &[u8]| -> String {
            let mut bytes = b"MCF1".to_vec();
            bytes.push(kind);
            bytes.extend_from_slice(&[0, 0, 0]);
            bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            bytes.extend_from_slice(payload);
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let proc_exit = |pid: u32, code: u32| frame(8, &ProcExitPayload { pid, code }.encode());

        let mut interleaved = String::from("OpenSBI\n");
        interleaved.push_str(&String::from_utf8_lossy(PAYLOAD_READY_FRAME));
        interleaved.push_str(&frame(6, b"MiniOS sched: spawned pid=0 name=spin\n"));
        interleaved.push_str(&frame(6, b"MiniOS sched: spawned pid=1 name=quick\n"));
        interleaved.push_str(&frame(2, b"a1\n"));
        interleaved.push_str(&frame(2, b"b1\nb2\nb3\n"));
        interleaved.push_str(&proc_exit(1, 7));
        interleaved.push_str(&frame(2, b"a2\na3\n"));
        interleaved.push_str(&proc_exit(0, 0));
        interleaved.push_str(&frame(
            6,
            b"\r\nMiniOS payload: ok processes=2 switches=3\n",
        ));
        assert_eq!(
            verify_sched_result(TEST_COMMAND, Some(0), &interleaved).map(|_| ()),
            Ok(())
        );

        // 逐次実行: process Aが完走してからB — プリエンプションは起きていない。
        let mut sequential = String::from("OpenSBI\n");
        sequential.push_str(&String::from_utf8_lossy(PAYLOAD_READY_FRAME));
        sequential.push_str(&frame(2, b"a1\na2\na3\n"));
        sequential.push_str(&frame(2, b"b1\nb2\nb3\n"));
        sequential.push_str(&proc_exit(0, 0));
        sequential.push_str(&proc_exit(1, 7));
        sequential.push_str(&frame(
            6,
            b"\r\nMiniOS payload: ok processes=2 switches=0\n",
        ));
        assert!(matches!(
            verify_sched_result(TEST_COMMAND, Some(0), &sequential),
            Err(QemuError::PayloadFrames { .. })
        ));

        // 交差していてもswitches=0ならtimer切り替えの証拠にならない。
        let mut no_switches = interleaved.clone();
        no_switches = no_switches.replace("switches=3", "switches=0");
        assert!(matches!(
            verify_sched_result(TEST_COMMAND, Some(0), &no_switches),
            Err(QemuError::PayloadFrames { .. })
        ));
    }

    // Catches a sched-io run where the reader never blocked (r2 before b3
    // would mean input arrived instantly, which the marker-triggered write
    // makes impossible), a missing ProcExit, or a missing processes=3 report.
    #[test]
    fn sched_io_verification_requires_blocked_window_and_all_exits() {
        let frame = |kind: u8, payload: &[u8]| -> String {
            let mut bytes = b"MCF1".to_vec();
            bytes.push(kind);
            bytes.extend_from_slice(&[0, 0, 0]);
            bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            bytes.extend_from_slice(payload);
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let proc_exit = |pid: u32, code: u32| frame(8, &ProcExitPayload { pid, code }.encode());

        // readerがblock中にquickとspinが進み、stdin到着後にr2が出る列。
        let mut blocked = String::from("OpenSBI\n");
        blocked.push_str(&String::from_utf8_lossy(PAYLOAD_READY_FRAME));
        blocked.push_str(&frame(2, b"r1\n"));
        blocked.push_str(&frame(2, b"b1\nb2\nb3\n"));
        blocked.push_str(&proc_exit(2, 7));
        blocked.push_str(&frame(2, b"a1\n"));
        blocked.push_str(&frame(2, b"r2\n"));
        blocked.push_str(&proc_exit(0, 5));
        blocked.push_str(&frame(2, b"a2\na3\n"));
        blocked.push_str(&proc_exit(1, 0));
        blocked.push_str(&frame(
            6,
            b"\r\nMiniOS payload: ok processes=3 switches=5\n",
        ));
        assert_eq!(
            verify_sched_io_result(TEST_COMMAND, Some(0), &blocked).map(|_| ()),
            Ok(())
        );

        // r2がb3より前にある＝readerがblockせず走り切った列は弾く。
        let mut not_blocked = String::from("OpenSBI\n");
        not_blocked.push_str(&String::from_utf8_lossy(PAYLOAD_READY_FRAME));
        not_blocked.push_str(&frame(2, b"r1\nr2\n"));
        not_blocked.push_str(&frame(2, b"b1\nb2\nb3\n"));
        not_blocked.push_str(&proc_exit(2, 7));
        not_blocked.push_str(&frame(2, b"a1\na2\na3\n"));
        not_blocked.push_str(&proc_exit(1, 0));
        not_blocked.push_str(&proc_exit(0, 5));
        not_blocked.push_str(&frame(
            6,
            b"\r\nMiniOS payload: ok processes=3 switches=3\n",
        ));
        assert!(matches!(
            verify_sched_io_result(TEST_COMMAND, Some(0), &not_blocked),
            Err(QemuError::PayloadFrames { .. })
        ));

        // ProcExitが欠けた列は弾く。
        let mut missing_exit = blocked.clone();
        let exit_pos = missing_exit.find(&proc_exit(1, 0)).expect("exit frame");
        missing_exit.replace_range(exit_pos..exit_pos + proc_exit(1, 0).len(), "");
        assert!(matches!(
            verify_sched_io_result(TEST_COMMAND, Some(0), &missing_exit),
            Err(QemuError::PayloadFrames { .. })
        ));
    }

    fn complete_payload_output() -> String {
        let mut output = "OpenSBI\n[ok] traps\n".to_owned();
        for frame in PAYLOAD_EXPECTED_FRAMES {
            output.push_str(&String::from_utf8_lossy(frame));
        }
        output
    }

    fn shell_quote_path(path: &Path) -> String {
        path.display().to_string().replace('\'', "'\\''")
    }
}
