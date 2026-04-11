use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "claude9",
    version,
    about = "Spawn run9 boxes preloaded for claude-based dev"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Ensure ./.claude9/config.toml exists and open it in $EDITOR
    Config,
    /// Create a new box from the base snap, sync repos, optionally run a task
    Spawn(SpawnArgs),
    /// Run a one-shot `claude -p` task on an existing box
    Task(TaskArgs),
    /// Resume the last saved claude session on a box with a follow-up
    Resume(ResumeArgs),
}

#[derive(Args)]
pub struct SpawnArgs {
    /// Explicit box id; omit to let portal-api allocate one
    #[arg(long)]
    pub name: Option<String>,

    /// Inline task prompt to run after the box is ready
    #[arg(long)]
    pub task: Option<String>,

    /// Read task prompt from a file
    #[arg(long = "task-file")]
    pub task_file: Option<PathBuf>,

    /// Skip git pull of predefined repos
    #[arg(long = "no-update")]
    pub no_update: bool,

    /// Override the base box from config
    #[arg(long = "base-box")]
    pub base_box: Option<String>,

    /// Override box shape from config
    #[arg(long)]
    pub shape: Option<String>,
}

#[derive(Args)]
pub struct TaskArgs {
    /// Target box id
    pub box_id: String,

    /// Inline prompt (joined with spaces)
    #[arg(trailing_var_arg = true)]
    pub prompt: Vec<String>,

    /// Read prompt from file instead of positional args
    #[arg(short, long)]
    pub file: Option<PathBuf>,
}

#[derive(Args)]
pub struct ResumeArgs {
    /// Target box id
    pub box_id: String,

    /// Inline follow-up prompt (joined with spaces)
    #[arg(trailing_var_arg = true)]
    pub prompt: Vec<String>,

    /// Read follow-up from file instead of positional args
    #[arg(short, long)]
    pub file: Option<PathBuf>,
}
