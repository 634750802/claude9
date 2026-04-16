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
    /// Spawn-or-reuse a named box and drop into an interactive claude session
    Talk(TalkArgs),
    /// Drop into `/bin/bash` on a run9 box (defaults to the configured base box)
    Bash(BashArgs),
    /// Re-attach to a running background task on a box
    Join(JoinArgs),
    /// Stop a running background task on a box
    Stop(StopArgs),
    /// List background tasks
    Ps,
}

#[derive(Args)]
pub struct SpawnArgs {
    /// Name prefix for the box (a random suffix is appended, e.g. mybox-a1b2c3d4)
    #[arg(long)]
    pub name: Option<String>,

    /// Short description of what this box is for (stored as claude9-task label)
    #[arg(long)]
    pub desc: Option<String>,

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
pub struct BashArgs {
    /// Box name / id (defaults to `defaults.base_box` from config.toml)
    pub box_name: Option<String>,

    /// Extra args passed to `/bin/bash` after `--`
    /// (e.g. `claude9 bash -- -lc 'echo hi'`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub bash_args: Vec<String>,
}

#[derive(Args)]
pub struct TalkArgs {
    /// Name prefix for the box: reuse an existing `<prefix>-*` or spawn fresh
    #[arg(long)]
    pub name: Option<String>,

    /// Seed the interactive session with a first user message
    #[arg(long = "first-prompt")]
    pub first_prompt: Option<String>,

    /// Read the first prompt from a file
    #[arg(long = "first-prompt-file")]
    pub first_prompt_file: Option<PathBuf>,

    /// Override `[claude].model` for this session
    #[arg(long)]
    pub model: Option<String>,

    /// Override `[claude].effort` for this session
    #[arg(long)]
    pub effort: Option<String>,

    /// When spawning a new box, pass this through to `claude9 spawn --desc`
    #[arg(long)]
    pub desc: Option<String>,

    /// Override box shape from config when spawning a new box
    /// (ignored when reusing an existing box, which can't be resized)
    #[arg(long)]
    pub shape: Option<String>,
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

#[derive(Args)]
pub struct JoinArgs {
    /// Target box id
    pub box_id: String,
}

#[derive(Args)]
pub struct StopArgs {
    /// Target box id
    pub box_id: String,
}
