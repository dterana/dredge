mod cli;
mod model;
mod scan;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Scan(args) => scan::run(&args),
    };
    match result {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(2)),
        Err(err) => {
            eprintln!("dredge: {err:#}");
            ExitCode::from(2)
        }
    }
}
