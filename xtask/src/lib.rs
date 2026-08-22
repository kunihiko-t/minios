pub mod cargo;
pub mod cli;
pub mod qemu;
pub mod tools;

use std::fmt;

use cli::Command;

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

pub fn run(command: Command) -> Result<(), XtaskError> {
    match command {
        Command::Setup => tools::check_setup()?,
        Command::Build => {
            cargo::build_kernel(false)?;
        }
        Command::Run => qemu::run_kernel()?,
        Command::TestBoot => {
            let transcript =
                qemu::run_test(qemu::TestKind::Boot, std::time::Duration::from_secs(5))?;
            print!("{transcript}");
        }
        Command::TestTimer => {
            let transcript =
                qemu::run_test(qemu::TestKind::Timer, std::time::Duration::from_secs(5))?;
            print!("{transcript}");
        }
        Command::TestTrap => {
            let transcript =
                qemu::run_test(qemu::TestKind::Trap, std::time::Duration::from_secs(5))?;
            print!("{transcript}");
        }
    }
    Ok(())
}
