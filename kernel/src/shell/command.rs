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
}
