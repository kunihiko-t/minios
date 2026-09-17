#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command<'a> {
    Empty,
    Help,
    Info,
    Uptime,
    Memory,
    Clear,
    Shutdown,
    #[cfg(any(test, target_arch = "riscv32"))]
    Echo(&'a str),
    #[cfg(any(test, target_arch = "riscv32", target_arch = "riscv64"))]
    Ls(&'a str),
    #[cfg(any(test, target_arch = "riscv32", target_arch = "riscv64"))]
    Cat(&'a str),
    #[cfg(any(test, target_arch = "riscv64"))]
    Rm(&'a str),
    #[cfg(any(test, target_arch = "riscv64"))]
    Mkdir(&'a str),
    #[cfg(any(test, target_arch = "riscv64"))]
    Rmdir(&'a str),
    Unknown(&'a str),
}

pub fn parse_command(input: &str) -> Command<'_> {
    let input = input.trim_matches(|character: char| character.is_ascii_whitespace());
    match input {
        "" => Command::Empty,
        "help" => Command::Help,
        "info" => Command::Info,
        "uptime" => Command::Uptime,
        "memory" => Command::Memory,
        "clear" => Command::Clear,
        "shutdown" => Command::Shutdown,
        #[cfg(any(test, target_arch = "riscv32"))]
        "echo" => Command::Echo(""),
        #[cfg(any(test, target_arch = "riscv32"))]
        input if input.starts_with("echo ") => Command::Echo(input[5..].trim_start_matches(' ')),
        #[cfg(any(test, target_arch = "riscv32", target_arch = "riscv64"))]
        "ls" => Command::Ls(""),
        #[cfg(any(test, target_arch = "riscv32", target_arch = "riscv64"))]
        "cat" => Command::Cat(""),
        #[cfg(any(test, target_arch = "riscv64"))]
        "rm" => Command::Rm(""),
        #[cfg(any(test, target_arch = "riscv64"))]
        input if input.starts_with("rm ") => Command::Rm(input[3..].trim_start_matches(' ')),
        #[cfg(any(test, target_arch = "riscv64"))]
        "mkdir" => Command::Mkdir(""),
        #[cfg(any(test, target_arch = "riscv64"))]
        input if input.starts_with("mkdir ") => Command::Mkdir(input[6..].trim_start_matches(' ')),
        #[cfg(any(test, target_arch = "riscv64"))]
        "rmdir" => Command::Rmdir(""),
        #[cfg(any(test, target_arch = "riscv64"))]
        input if input.starts_with("rmdir ") => Command::Rmdir(input[6..].trim_start_matches(' ')),
        #[cfg(any(test, target_arch = "riscv32", target_arch = "riscv64"))]
        input => {
            if let Some(argument) = input.strip_prefix("ls ") {
                Command::Ls(argument.trim_start_matches(' '))
            } else if let Some(argument) = input.strip_prefix("cat ") {
                Command::Cat(argument.trim_start_matches(' '))
            } else {
                Command::Unknown(input)
            }
        }
        #[cfg(not(any(test, target_arch = "riscv32", target_arch = "riscv64")))]
        unknown => Command::Unknown(unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, parse_command};

    #[test]
    fn parser_trims_ascii_whitespace_before_matching() {
        assert_eq!(parse_command("  uptime  "), Command::Uptime);
    }

    #[test]
    fn parser_recognizes_memory_command() {
        assert_eq!(parse_command("memory"), Command::Memory);
    }

    #[test]
    fn parser_preserves_trimmed_unknown_input() {
        assert_eq!(parse_command("wat"), Command::Unknown("wat"));
    }

    #[test]
    fn parser_recognizes_the_remaining_supported_commands() {
        assert_eq!(parse_command("help"), Command::Help);
        assert_eq!(parse_command("info"), Command::Info);
        assert_eq!(parse_command("clear"), Command::Clear);
        assert_eq!(parse_command("shutdown"), Command::Shutdown);
    }

    #[test]
    fn parser_distinguishes_empty_input_from_unknown_input() {
        assert_eq!(parse_command(" \t"), Command::Empty);
        assert_eq!(parse_command("HELP"), Command::Unknown("HELP"));
    }

    #[test]
    fn parser_treats_echo_payload_as_a_command() {
        assert_eq!(parse_command("echo hello"), Command::Echo("hello"));
    }

    #[test]
    fn parser_treats_bare_echo_as_a_command() {
        assert_eq!(parse_command("echo"), Command::Echo(""));
    }

    #[test]
    fn parser_does_not_match_echo_prefixes() {
        assert_eq!(parse_command("echoes"), Command::Unknown("echoes"));
    }

    #[test]
    fn parser_recognizes_rv32_storage_commands() {
        assert_eq!(parse_command("ls"), Command::Ls(""));
        assert_eq!(parse_command("ls DOCS"), Command::Ls("DOCS"));
        assert_eq!(parse_command("cat HELLO.TXT"), Command::Cat("HELLO.TXT"));
        assert_eq!(
            parse_command("cat DOCS/NOTE.TXT"),
            Command::Cat("DOCS/NOTE.TXT")
        );
    }

    #[test]
    fn parser_preserves_an_empty_cat_argument_for_a_usage_error() {
        assert_eq!(parse_command("cat"), Command::Cat(""));
        assert_eq!(parse_command("cat    "), Command::Cat(""));
    }

    #[test]
    fn parser_does_not_match_storage_command_prefixes() {
        assert_eq!(parse_command("listing"), Command::Unknown("listing"));
        assert_eq!(parse_command("catalog"), Command::Unknown("catalog"));
    }

    #[test]
    fn parser_recognizes_rm_with_and_without_an_argument() {
        assert_eq!(parse_command("rm"), Command::Rm(""));
        assert_eq!(parse_command("rm OLD.TXT"), Command::Rm("OLD.TXT"));
        assert_eq!(
            parse_command("rm DOCS/NOTE.TXT"),
            Command::Rm("DOCS/NOTE.TXT")
        );
    }

    #[test]
    fn parser_recognizes_mkdir_and_rmdir_with_and_without_arguments() {
        assert_eq!(parse_command("mkdir"), Command::Mkdir(""));
        assert_eq!(parse_command("mkdir NEWDIR"), Command::Mkdir("NEWDIR"));
        assert_eq!(parse_command("rmdir"), Command::Rmdir(""));
        assert_eq!(parse_command("rmdir OLD"), Command::Rmdir("OLD"));
        // `rm`/`rmdir`のprefix共有を取り違えない。
        assert_eq!(parse_command("rm X"), Command::Rm("X"));
        assert_eq!(parse_command("mkdirt"), Command::Unknown("mkdirt"));
    }
}
