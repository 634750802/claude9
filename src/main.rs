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
        cli::Command::Spawn(args) => commands::spawn(args).map(|_| ()),
        cli::Command::Task(args) => commands::task(args),
        cli::Command::Resume(args) => commands::resume(args),
        cli::Command::Talk(args) => commands::talk(args),
        cli::Command::Bash(args) => commands::bash(args),
        cli::Command::Join(args) => commands::join(args),
        cli::Command::Stop(args) => commands::stop(args),
        cli::Command::Ps => commands::ps(),
    }
}
