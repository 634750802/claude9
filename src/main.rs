mod cli;
mod commands;
mod config;
mod resolver;
mod run9;
mod state;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Config => commands::config_cmd(),
        cli::Command::Spawn(args) => commands::spawn(args),
        cli::Command::Task(args) => commands::task(args),
        cli::Command::Resume(args) => commands::resume(args),
    }
}
