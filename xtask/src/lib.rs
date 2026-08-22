pub mod cli;
pub mod tools;

use std::fmt;

use cli::Command;

#[derive(Debug)]
pub enum XtaskError {
    Tool(tools::ToolError),
}

impl fmt::Display for XtaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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

pub fn run(command: Command) -> Result<(), XtaskError> {
    match command {
        Command::Setup => tools::check_setup()?,
    }
    Ok(())
}
