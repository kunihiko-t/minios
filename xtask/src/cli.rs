use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Setup,
    Build,
    Run,
    Bundle(BundleOptions),
    Test(TestFilter),
    Check,
}

/// `cargo xtask bundle`の入力。`None`の項目はbundle側の既定値を使う。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundleOptions {
    pub name: Option<String>,
    pub args: Vec<String>,
    pub output: Option<PathBuf>,
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
    UserSyscall,
    UserExit,
    Payload,
    PayloadArgs,
    PayloadStdin,
    Shell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    MissingCommand,
    UnknownCommand(String),
    InvalidBundleOptions(String),
}

pub fn help() -> &'static str {
    "MiniOS development commands:\n\
  cargo xtask setup\n\
  cargo xtask build\n\
  cargo xtask run\n\
  cargo xtask bundle [--name <name>] [--arg <value>]... [--output <path>]\n\
  cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|user-syscall|user-exit|payload|payload-args|payload-stdin|shell]\n\
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
        [command, test] if command == "test" && test == "user-syscall" => {
            Ok(Command::Test(TestFilter::UserSyscall))
        }
        [command, test] if command == "test" && test == "user-exit" => {
            Ok(Command::Test(TestFilter::UserExit))
        }
        [command, test] if command == "test" && test == "payload" => {
            Ok(Command::Test(TestFilter::Payload))
        }
        [command, test] if command == "test" && test == "payload-args" => {
            Ok(Command::Test(TestFilter::PayloadArgs))
        }
        [command, test] if command == "test" && test == "payload-stdin" => {
            Ok(Command::Test(TestFilter::PayloadStdin))
        }
        [command, test] if command == "test" && test == "shell" => {
            Ok(Command::Test(TestFilter::Shell))
        }
        [command] if command == "check" => Ok(Command::Check),
        [command, options @ ..] if command == "bundle" => {
            parse_bundle_options(options).map(Command::Bundle)
        }
        [] => Err(CliError::MissingCommand),
        [command, ..] => Err(CliError::UnknownCommand(command.clone())),
    }
}

/// `bundle`以降のoption列をparseする。`--arg`だけが繰り返し可能で、
/// 値の意味検査 (文字種や上限) はbundle生成側の責務である。
fn parse_bundle_options(options: &[String]) -> Result<BundleOptions, CliError> {
    let mut parsed = BundleOptions::default();
    let mut index = 0;
    while index < options.len() {
        let flag = options[index].as_str();
        match flag {
            "--name" | "--arg" | "--output" => {
                let Some(value) = options.get(index + 1) else {
                    return Err(CliError::InvalidBundleOptions(format!(
                        "bundle option {flag} requires a value"
                    )));
                };
                match flag {
                    "--name" => {
                        if parsed.name.replace(value.clone()).is_some() {
                            return Err(CliError::InvalidBundleOptions(
                                "duplicate bundle option: --name".to_owned(),
                            ));
                        }
                    }
                    "--arg" => parsed.args.push(value.clone()),
                    _ => {
                        if parsed.output.replace(PathBuf::from(value)).is_some() {
                            return Err(CliError::InvalidBundleOptions(
                                "duplicate bundle option: --output".to_owned(),
                            ));
                        }
                    }
                }
                index += 2;
            }
            other => {
                return Err(CliError::InvalidBundleOptions(format!(
                    "unknown bundle option: {other}"
                )));
            }
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn parses_all_public_commands() {
        assert_eq!(parse(&owned(&["setup"])), Ok(Command::Setup));
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
            (vec!["test", "user-syscall"], TestFilter::UserSyscall),
            (vec!["test", "user-exit"], TestFilter::UserExit),
            (vec!["test", "payload"], TestFilter::Payload),
            (vec!["test", "payload-args"], TestFilter::PayloadArgs),
            (vec!["test", "payload-stdin"], TestFilter::PayloadStdin),
            (vec!["test", "shell"], TestFilter::Shell),
        ] {
            assert_eq!(parse(&owned(&args)), Ok(Command::Test(expected)));
        }
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
            "cargo xtask bundle [--name <name>] [--arg <value>]... [--output <path>]",
            "cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|user-syscall|user-exit|payload|payload-args|payload-stdin|shell]",
            "cargo xtask check",
        ] {
            assert!(help.contains(command), "missing help entry: {command}");
        }
    }

    #[test]
    fn parses_bundle_with_defaults() {
        assert_eq!(
            parse(&owned(&["bundle"])),
            Ok(Command::Bundle(BundleOptions::default()))
        );
    }

    #[test]
    fn parses_bundle_with_name_repeated_args_and_output() {
        assert_eq!(
            parse(&owned(&[
                "bundle",
                "--name",
                "hello",
                "--arg",
                "alpha",
                "--arg",
                "bravo",
                "--output",
                "target/hello.mcb",
            ])),
            Ok(Command::Bundle(BundleOptions {
                name: Some("hello".to_owned()),
                args: vec!["alpha".to_owned(), "bravo".to_owned()],
                output: Some(PathBuf::from("target/hello.mcb")),
            }))
        );
    }

    #[test]
    fn rejects_bundle_option_without_a_value() {
        assert_eq!(
            parse(&owned(&["bundle", "--name"])),
            Err(CliError::InvalidBundleOptions(
                "bundle option --name requires a value".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_unknown_bundle_option_by_name() {
        assert_eq!(
            parse(&owned(&["bundle", "--compress"])),
            Err(CliError::InvalidBundleOptions(
                "unknown bundle option: --compress".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_duplicate_bundle_name_and_output() {
        assert_eq!(
            parse(&owned(&["bundle", "--name", "a", "--name", "b"])),
            Err(CliError::InvalidBundleOptions(
                "duplicate bundle option: --name".to_owned()
            ))
        );
        assert_eq!(
            parse(&owned(&["bundle", "--output", "a", "--output", "b"])),
            Err(CliError::InvalidBundleOptions(
                "duplicate bundle option: --output".to_owned()
            ))
        );
    }
}
