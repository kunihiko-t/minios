pub mod cargo;
pub mod cli;
pub mod qemu;
pub mod tools;

use std::{fmt, io::Write, time::Instant};

use cli::{Command, TestFilter};

#[derive(Debug)]
pub enum XtaskError {
    Cargo(cargo::CargoError),
    Qemu(qemu::QemuError),
    Tool(tools::ToolError),
}

impl fmt::Display for XtaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cargo(error) => error.fmt(formatter),
            Self::Qemu(error) => error.fmt(formatter),
            Self::Tool(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for XtaskError {}

impl From<tools::ToolError> for XtaskError {
    fn from(error: tools::ToolError) -> Self {
        Self::Tool(error)
    }
}

impl From<cargo::CargoError> for XtaskError {
    fn from(error: cargo::CargoError) -> Self {
        Self::Cargo(error)
    }
}

impl From<qemu::QemuError> for XtaskError {
    fn from(error: qemu::QemuError) -> Self {
        Self::Qemu(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Format,
    ClippyXtask,
    ClippyKernelLib,
    ClippyKernelBin,
    BuildKernel,
    KernelUnitTests,
    XtaskUnitTests,
    Qemu(qemu::TestKind),
}

impl Phase {
    fn cargo_args(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Format => Some(&["fmt", "--all", "--", "--check"]),
            Self::ClippyXtask => Some(&[
                "clippy",
                "-p",
                "xtask",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]),
            Self::ClippyKernelLib => Some(&[
                "clippy",
                "-p",
                "minios-kernel",
                "--lib",
                "--",
                "-D",
                "warnings",
            ]),
            Self::ClippyKernelBin => Some(&[
                "clippy",
                "-p",
                "minios-kernel",
                "--bin",
                "minios-kernel",
                "--target",
                "riscv64gc-unknown-none-elf",
                "--",
                "-D",
                "warnings",
            ]),
            Self::BuildKernel => Some(&[
                "build",
                "-p",
                "minios-kernel",
                "--bin",
                "minios-kernel",
                "--target",
                "riscv64gc-unknown-none-elf",
            ]),
            Self::KernelUnitTests => Some(&["test", "-p", "minios-kernel", "--lib"]),
            Self::XtaskUnitTests => Some(&["test", "-p", "xtask"]),
            Self::Qemu(_) => None,
        }
    }

    fn command(self) -> String {
        if let Some(args) = self.cargo_args() {
            return format!("cargo {}", args.join(" "));
        }
        match self {
            Self::Qemu(qemu::TestKind::Boot) => "QEMU boot test".to_owned(),
            Self::Qemu(qemu::TestKind::Trap) => "QEMU trap test".to_owned(),
            Self::Qemu(qemu::TestKind::Timer) => "QEMU timer test".to_owned(),
            Self::Qemu(qemu::TestKind::Memory) => "QEMU memory test".to_owned(),
            Self::Qemu(qemu::TestKind::Shell) => "QEMU shell test".to_owned(),
            _ => unreachable!("Cargo phases returned above"),
        }
    }
}

fn test_phases() -> Vec<Phase> {
    vec![
        Phase::KernelUnitTests,
        Phase::XtaskUnitTests,
        Phase::Qemu(qemu::TestKind::Boot),
        Phase::Qemu(qemu::TestKind::Trap),
        Phase::Qemu(qemu::TestKind::Timer),
        Phase::Qemu(qemu::TestKind::Memory),
        Phase::Qemu(qemu::TestKind::Shell),
    ]
}

fn check_phases() -> Vec<Phase> {
    vec![
        Phase::Format,
        Phase::ClippyXtask,
        Phase::ClippyKernelLib,
        Phase::ClippyKernelBin,
        Phase::BuildKernel,
        Phase::KernelUnitTests,
        Phase::XtaskUnitTests,
        Phase::Qemu(qemu::TestKind::Boot),
        Phase::Qemu(qemu::TestKind::Trap),
        Phase::Qemu(qemu::TestKind::Timer),
        Phase::Qemu(qemu::TestKind::Memory),
        Phase::Qemu(qemu::TestKind::Shell),
    ]
}

fn run_phases<E>(
    phases: &[Phase],
    output: &mut impl Write,
    mut action: impl FnMut(Phase) -> Result<String, E>,
) -> Result<(), E> {
    let all_started = Instant::now();
    for (index, phase) in phases.iter().copied().enumerate() {
        let number = index + 1;
        let _ = writeln!(output, "[{number}/{}] {}", phases.len(), phase.command());
        let started = Instant::now();
        match action(phase) {
            Ok(transcript) => {
                if !transcript.is_empty() {
                    let _ = write!(output, "{transcript}");
                    if !transcript.ends_with('\n') {
                        let _ = writeln!(output);
                    }
                }
                let _ = writeln!(
                    output,
                    "phase {number}/{} passed (elapsed: {:.3}s)",
                    phases.len(),
                    started.elapsed().as_secs_f64()
                );
            }
            Err(error) => {
                let _ = writeln!(
                    output,
                    "phase {number}/{} failed (elapsed: {:.3}s)",
                    phases.len(),
                    started.elapsed().as_secs_f64()
                );
                let _ = writeln!(
                    output,
                    "summary: FAILED at phase {number}/{}; {index} passed, 1 failed (elapsed: {:.3}s)",
                    phases.len(),
                    all_started.elapsed().as_secs_f64()
                );
                return Err(error);
            }
        }
    }
    let _ = writeln!(
        output,
        "summary: PASSED all {} phases (elapsed: {:.3}s)",
        phases.len(),
        all_started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn execute_phase(phase: Phase) -> Result<String, XtaskError> {
    if let Some(args) = phase.cargo_args() {
        return cargo::run(args).map_err(XtaskError::Cargo);
    }
    let Phase::Qemu(kind) = phase else {
        unreachable!("Cargo phases returned above")
    };
    // 起動遅延は許容しつつ、停止したゲストを早期に診断できる統一期限を使う。
    qemu::run_test(kind, std::time::Duration::from_secs(5)).map_err(XtaskError::Qemu)
}

fn run_phase_plan(phases: &[Phase]) -> Result<(), XtaskError> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    run_phases(phases, &mut output, execute_phase)
}

fn phase_plan_for(command: &Command) -> Option<Vec<Phase>> {
    match command {
        Command::Test(TestFilter::All) => Some(test_phases()),
        Command::Test(TestFilter::Boot) => Some(vec![Phase::Qemu(qemu::TestKind::Boot)]),
        Command::Test(TestFilter::Trap) => Some(vec![Phase::Qemu(qemu::TestKind::Trap)]),
        Command::Test(TestFilter::Timer) => Some(vec![Phase::Qemu(qemu::TestKind::Timer)]),
        Command::Test(TestFilter::Memory) => Some(vec![Phase::Qemu(qemu::TestKind::Memory)]),
        Command::Test(TestFilter::Shell) => Some(vec![Phase::Qemu(qemu::TestKind::Shell)]),
        Command::Check => Some(check_phases()),
        Command::Setup | Command::Build | Command::Run => None,
    }
}

pub fn run(command: Command) -> Result<(), XtaskError> {
    if let Some(phases) = phase_plan_for(&command) {
        return run_phase_plan(&phases);
    }
    match command {
        Command::Setup => tools::check_setup()?,
        Command::Build => {
            cargo::build_kernel(false)?;
        }
        Command::Run => qemu::run_kernel()?,
        Command::Test(_) | Command::Check => unreachable!("phase commands returned above"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_orders_host_then_every_qemu_path() {
        assert_eq!(
            test_phases(),
            vec![
                Phase::KernelUnitTests,
                Phase::XtaskUnitTests,
                Phase::Qemu(qemu::TestKind::Boot),
                Phase::Qemu(qemu::TestKind::Trap),
                Phase::Qemu(qemu::TestKind::Timer),
                Phase::Qemu(qemu::TestKind::Memory),
                Phase::Qemu(qemu::TestKind::Shell),
            ]
        );
    }

    #[test]
    fn public_test_all_command_connects_to_the_complete_test_plan() {
        assert_eq!(
            phase_plan_for(&Command::Test(TestFilter::All)),
            Some(test_phases())
        );
    }

    #[test]
    fn phase_runner_prints_a_success_summary() {
        let phases = [Phase::Format, Phase::BuildKernel];
        let mut output = Vec::new();

        run_phases(&phases, &mut output, |_| Ok::<_, ()>(String::new()))
            .expect("all successful actions must pass");

        let output = String::from_utf8(output).expect("phase output must be UTF-8");
        assert!(output.contains("summary: PASSED all 2 phases (elapsed:"));
    }

    #[test]
    fn check_orders_compiler_operations_then_host_and_qemu_tests() {
        assert_eq!(
            check_phases(),
            vec![
                Phase::Format,
                Phase::ClippyXtask,
                Phase::ClippyKernelLib,
                Phase::ClippyKernelBin,
                Phase::BuildKernel,
                Phase::KernelUnitTests,
                Phase::XtaskUnitTests,
                Phase::Qemu(qemu::TestKind::Boot),
                Phase::Qemu(qemu::TestKind::Trap),
                Phase::Qemu(qemu::TestKind::Timer),
                Phase::Qemu(qemu::TestKind::Memory),
                Phase::Qemu(qemu::TestKind::Shell),
            ]
        );
    }

    #[test]
    fn phase_runner_stops_at_first_failure_and_summarizes_it() {
        let phases = [Phase::Format, Phase::ClippyXtask, Phase::BuildKernel];
        let mut invoked = Vec::new();
        let mut output = Vec::new();

        let result = run_phases(&phases, &mut output, |phase| {
            invoked.push(phase);
            if phase == Phase::ClippyXtask {
                Err("clippy failed")
            } else {
                Ok(String::new())
            }
        });

        assert_eq!(result, Err("clippy failed"));
        assert_eq!(invoked, vec![Phase::Format, Phase::ClippyXtask]);
        let output = String::from_utf8(output).expect("phase output must be UTF-8");
        assert!(output.contains("[1/3] cargo fmt --all -- --check"));
        assert!(output.contains("[2/3] cargo clippy -p xtask --all-targets -- -D warnings"));
        assert!(output.contains("elapsed:"));
        assert!(output.contains("summary: FAILED at phase 2/3; 1 passed, 1 failed"));
    }
}
