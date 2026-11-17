#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use clap::Parser;

use dredge::cli::{Cli, Command};
use dredge::{exit, report, scan};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Command::Scan(args) = cli.command;
    let rep = match scan::run(&args) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("dredge: {err:#}");
            return ExitCode::from(2);
        }
    };
    for note in &rep.notes {
        // A closed stderr must not abort the run.
        let _ = writeln!(std::io::stderr(), "dredge: note: {note}");
    }
    let out = if args.json {
        report::render_json(&rep)
    } else {
        let color = !args.no_color
            && std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal();
        report::render_human(&rep, color, args.all)
    };
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(out.as_bytes())
        .and_then(|_| stdout.flush())
        .is_err()
    {
        return ExitCode::from(2);
    }
    let code = exit::code(&rep, &args.fail_on);
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}
