#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Setup,
    Build,
    Run,
    Test(TestFilter),
    Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestFilter {
    All,
    Boot,
    Trap,
    Timer,
    Memory,
    Vm,
    Elf,
    UserEntry,
    UserTrap,
    Shell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    MissingCommand,
    UnknownCommand(String),
}

pub fn help() -> &'static str {
    "MiniOS development commands:\n\
  cargo xtask setup\n\
  cargo xtask build\n\
  cargo xtask run\n\
  cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|shell]\n\
  cargo xtask check"
}

pub fn parse(args: &[String]) -> Result<Command, CliError> {
    match args {
        [command] if command == "setup" => Ok(Command::Setup),
        [command] if command == "build" => Ok(Command::Build),
        [command] if command == "run" => Ok(Command::Run),
        [command] if command == "test" => Ok(Command::Test(TestFilter::All)),
        [command, test] if command == "test" && test == "all" => Ok(Command::Test(TestFilter::All)),
        [command, test] if command == "test" && test == "boot" => {
            Ok(Command::Test(TestFilter::Boot))
        }
        [command, test] if command == "test" && test == "trap" => {
            Ok(Command::Test(TestFilter::Trap))
        }
        [command, test] if command == "test" && test == "timer" => {
            Ok(Command::Test(TestFilter::Timer))
        }
        [command, test] if command == "test" && test == "memory" => {
            Ok(Command::Test(TestFilter::Memory))
        }
        [command, test] if command == "test" && test == "vm" => Ok(Command::Test(TestFilter::Vm)),
        [command, test] if command == "test" && test == "elf" => Ok(Command::Test(TestFilter::Elf)),
        [command, test] if command == "test" && test == "user-entry" => {
            Ok(Command::Test(TestFilter::UserEntry))
        }
        [command, test] if command == "test" && test == "user-trap" => {
            Ok(Command::Test(TestFilter::UserTrap))
        }
        [command, test] if command == "test" && test == "shell" => {
            Ok(Command::Test(TestFilter::Shell))
        }
        [command] if command == "check" => Ok(Command::Check),
        [] => Err(CliError::MissingCommand),
        [command, ..] => Err(CliError::UnknownCommand(command.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn parses_all_public_commands() {
        assert_eq!(parse(&owned(&["build"])), Ok(Command::Build));
        assert_eq!(parse(&owned(&["run"])), Ok(Command::Run));
        assert_eq!(parse(&owned(&["test"])), Ok(Command::Test(TestFilter::All)));
        assert_eq!(
            parse(&owned(&["test", "timer"])),
            Ok(Command::Test(TestFilter::Timer))
        );
        assert_eq!(parse(&owned(&["check"])), Ok(Command::Check));
    }

    #[test]
    fn parses_explicit_all_and_every_individual_test_filter() {
        for (args, expected) in [
            (vec!["test", "all"], TestFilter::All),
            (vec!["test", "boot"], TestFilter::Boot),
            (vec!["test", "trap"], TestFilter::Trap),
            (vec!["test", "timer"], TestFilter::Timer),
            (vec!["test", "memory"], TestFilter::Memory),
            (vec!["test", "vm"], TestFilter::Vm),
            (vec!["test", "elf"], TestFilter::Elf),
            (vec!["test", "user-entry"], TestFilter::UserEntry),
            (vec!["test", "user-trap"], TestFilter::UserTrap),
            (vec!["test", "shell"], TestFilter::Shell),
        ] {
            assert_eq!(parse(&owned(&args)), Ok(Command::Test(expected)));
        }
    }

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
            Ok(Command::Test(TestFilter::Boot))
        );
        assert_eq!(
            parse(&["test".to_owned(), "timer".to_owned()]),
            Ok(Command::Test(TestFilter::Timer))
        );
        assert_eq!(
            parse(&["test".to_owned(), "trap".to_owned()]),
            Ok(Command::Test(TestFilter::Trap))
        );
        assert_eq!(
            parse(&["test".to_owned(), "memory".to_owned()]),
            Ok(Command::Test(TestFilter::Memory))
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

    #[test]
    fn help_advertises_every_public_command_and_test_filter() {
        let help = help();

        for command in [
            "cargo xtask setup",
            "cargo xtask build",
            "cargo xtask run",
            "cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|shell]",
            "cargo xtask check",
        ] {
            assert!(help.contains(command), "missing help entry: {command}");
        }
    }

    #[test]
    fn parses_shell_test_command() {
        assert_eq!(
            parse(&["test".to_owned(), "shell".to_owned()]),
            Ok(Command::Test(TestFilter::Shell))
        );
    }
}
