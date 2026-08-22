#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Setup,
    Build,
    Run,
    TestBoot,
    TestTimer,
    TestTrap,
    TestMemory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    MissingCommand,
    UnknownCommand(String),
}

pub fn parse(args: &[String]) -> Result<Command, CliError> {
    match args {
        [command] if command == "setup" => Ok(Command::Setup),
        [command] if command == "build" => Ok(Command::Build),
        [command] if command == "run" => Ok(Command::Run),
        [command, test] if command == "test" && test == "boot" => Ok(Command::TestBoot),
        [command, test] if command == "test" && test == "timer" => Ok(Command::TestTimer),
        [command, test] if command == "test" && test == "trap" => Ok(Command::TestTrap),
        [command, test] if command == "test" && test == "memory" => Ok(Command::TestMemory),
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
    fn parses_kernel_build_run_and_test_commands() {
        assert_eq!(parse(&["build".to_owned()]), Ok(Command::Build));
        assert_eq!(parse(&["run".to_owned()]), Ok(Command::Run));
        assert_eq!(
            parse(&["test".to_owned(), "boot".to_owned()]),
            Ok(Command::TestBoot)
        );
        assert_eq!(
            parse(&["test".to_owned(), "timer".to_owned()]),
            Ok(Command::TestTimer)
        );
        assert_eq!(
            parse(&["test".to_owned(), "trap".to_owned()]),
            Ok(Command::TestTrap)
        );
        assert_eq!(
            parse(&["test".to_owned(), "memory".to_owned()]),
            Ok(Command::TestMemory)
        );
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
