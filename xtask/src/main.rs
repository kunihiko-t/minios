use xtask::{cli, run};

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let result = cli::parse(&args)
        .map_err(|error| error_message(&error))
        .and_then(|command| run(command).map_err(|error| error.to_string()));

    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn error_message(error: &cli::CliError) -> String {
    match error {
        cli::CliError::MissingCommand => format!("missing xtask command\n\n{}", cli::help()),
        cli::CliError::UnknownCommand(command) => {
            format!("unknown xtask command: {command}\n\n{}", cli::help())
        }
    }
}
