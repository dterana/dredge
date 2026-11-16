use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "dredge",
    version,
    about = "Report untracked or drifted dotfiles and secrets"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scan the home directory (read-only)
    Scan(ScanArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FailOn {
    Secrets,
    Drift,
    Untracked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LayoutArg {
    Auto,
    Stow,
    Chezmoi,
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Path to the dotfiles repository
    #[arg(long)]
    pub dotfiles: Option<PathBuf>,

    /// Extra root directory to scan (repeatable)
    #[arg(long)]
    pub root: Vec<PathBuf>,

    /// Show everything, including tracked items
    #[arg(long)]
    pub all: bool,

    /// Emit JSON output
    #[arg(long)]
    pub json: bool,

    /// Disable colored output
    #[arg(long)]
    pub no_color: bool,

    /// Conditions that cause a non-zero exit (comma separated)
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "secrets,drift"
    )]
    pub fail_on: Vec<FailOn>,

    /// Dotfiles repository layout
    #[arg(long, value_enum, default_value = "auto")]
    pub layout: LayoutArg,

    /// Override the home directory (used by tests)
    #[arg(long, hide = true)]
    pub home: Option<PathBuf>,
}
