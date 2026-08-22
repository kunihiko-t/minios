#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command<'a> {
    Empty,
    Help,
    Info,
    Uptime,
    Memory,
    Clear,
    Shutdown,
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
}
