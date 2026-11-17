#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::io::{ErrorKind, IsTerminal, Write};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

use clap::Parser;

use dredge::cli::{Cli, Command};
use dredge::progress::{self, Progress};
use dredge::{exit, report, scan};

fn main() -> ExitCode {
    // A panic prints nothing of its own (its message could carry content);
    // it becomes one fixed line and exit code 2.
    scan::silence_panics();
    match panic::catch_unwind(AssertUnwindSafe(run)) {
        Ok(code) => code,
        Err(_) => {
            say("dredge: internal error: the scan stopped unexpectedly");
            ExitCode::from(2)
        }
    }
}

fn run() -> ExitCode {
    let cli = Cli::parse();
    let Command::Scan(args) = cli.command;
    let stderr = std::io::stderr();
    let show = !args.no_progress && stderr.is_terminal();
    let mut progress = Progress::new(Box::new(stderr), show, progress::MIN_INTERVAL);
    let rep = match scan::run_with_progress(&args, &mut progress) {
        Ok(r) => r,
        Err(err) => {
            say(&format!("dredge: {err:#}"));
            return ExitCode::from(2);
        }
    };
    for note in &rep.notes {
        say(&format!("dredge: note: {note}"));
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
    match stdout
        .write_all(out.as_bytes())
        .and_then(|_| stdout.flush())
    {
        Ok(()) => {}
        // The reader went away (for example `| head`): not a tool failure.
        Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
        Err(e) => {
            say(&format!("dredge: cannot write the report: {e}"));
            return ExitCode::from(2);
        }
    }
    let code = exit::code(&rep, &args.fail_on);
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}

/// One line on stderr. Unlike `eprintln!`, never panics when stderr is closed.
fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}
