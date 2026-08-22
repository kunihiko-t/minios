#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Setup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    MissingCommand,
    UnknownCommand(String),
}

pub fn parse(args: &[String]) -> Result<Command, CliError> {
    match args {
        [command] if command == "setup" => Ok(Command::Setup),
        [] => Err(CliError::MissingCommand),
        [command, ..] => Err(CliError::UnknownCommand(command.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_setup_command() {
        let args = vec!["setup".to_owned()];
        assert_eq!(parse(&args), Ok(Command::Setup));
    }

    #[test]
    fn rejects_unknown_command_with_helpful_name() {
        let args = vec!["unknown".to_owned()];
        assert_eq!(
            parse(&args),
            Err(CliError::UnknownCommand("unknown".to_owned()))
        );
    }
}
