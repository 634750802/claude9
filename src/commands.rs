use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use claude_codes::ClaudeOutput;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::{ResumeArgs, SpawnArgs, TaskArgs};
use crate::config::{self, ClaudeOptions, REMOTE_USER, REPOS_DIR, WORKSPACE};
use crate::resolver;
use crate::run9;
use crate::state::{self, BoxMeta};

pub fn config_cmd() -> Result<()> {
    let path = config::ensure_exists()?;
    println!("{}", path.display());
    if let Ok(editor) = std::env::var("EDITOR") {
        let _ = std::process::Command::new(editor).arg(&path).status();
    }
    Ok(())
}

pub fn spawn(args: SpawnArgs) -> Result<()> {
    let cfg = config::load()?;
    let base_box = args
        .base_box
        .clone()
        .unwrap_or_else(|| cfg.defaults.base_box.clone());
    let shape = args
        .shape
        .clone()
        .unwrap_or_else(|| cfg.defaults.shape.clone());

    println!("[claude9] resolving base snap from box '{}'", base_box);
    let snap_id = resolver::resolve_base_snap(&base_box)?;
    println!("[claude9] base snap: {}", snap_id);

    let whoami = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let labels = [
        ("claude9", "managed"),
        ("claude9-base", base_box.as_str()),
        ("claude9-owner", whoami.as_str()),
    ];

    println!("[claude9] creating box (shape={})", shape);
    let created = run9::box_create_from_snap(args.name.as_deref(), &snap_id, &shape, &labels)?;
    let box_id = extract_box_id(&created)?;
    println!("[claude9] box created: {}", box_id);

    wait_for_ready(&box_id, Duration::from_secs(180))?;
    println!("[claude9] box ready");

    // Ensure repos dir exists (cheap idempotent op in case base snap lacks it).
    run9::box_exec(
        &box_id,
        REMOTE_USER,
        WORKSPACE,
        &HashMap::new(),
        &["/bin/sh", "-lc", &format!("mkdir -p {}", REPOS_DIR)],
    )?;

    // Sync repos (serial; v1 doesn't do parallel).
    let mut project_names: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    if args.no_update {
        println!("[claude9] --no-update set, skipping repo sync");
        for p in &cfg.projects {
            project_names.push(p.local_name());
        }
    } else {
        for p in &cfg.projects {
            let local = p.local_name();
            project_names.push(local.clone());
            println!("[claude9] sync {} -> {}", p.repo, local);

            let mut env = HashMap::new();
            env.insert("C9_REPO".into(), p.repo.clone());
            env.insert("C9_NAME".into(), local.clone());

            let script = r#"
set -e
if [ -d "$C9_NAME/.git" ]; then
  git -C "$C9_NAME" fetch --all --prune
  git -C "$C9_NAME" pull --ff-only || echo "[claude9] non-ff pull for $C9_NAME"
else
  gh repo clone "$C9_REPO" "$C9_NAME"
fi
"#;

            match run9::box_exec(
                &box_id,
                REMOTE_USER,
                REPOS_DIR,
                &env,
                &["/bin/bash", "-lc", script],
            ) {
                Ok(res) => {
                    if !res.stdout.trim().is_empty() {
                        println!("{}", res.stdout.trim_end());
                    }
                    if !res.stderr.trim().is_empty() {
                        eprintln!("{}", res.stderr.trim_end());
                    }
                }
                Err(e) => {
                    eprintln!("[claude9] sync failed for {}: {}", p.repo, e);
                    failed.push(p.repo.clone());
                }
            }
        }
    }

    // Persist meta.
    let meta = BoxMeta {
        box_id: box_id.clone(),
        base_box: base_box.clone(),
        snap_id: snap_id.clone(),
        shape: shape.clone(),
        created_at: Utc::now(),
        projects: project_names,
    };
    state::save_meta(&meta)?;

    if !failed.is_empty() {
        eprintln!("[claude9] repo sync failures: {:?}", failed);
    }

    // Optional inline task.
    let prompt = resolve_prompt(args.task.clone(), args.task_file.as_deref())?;
    if let Some(p) = prompt {
        run_claude_task(&box_id, &p, None, &cfg.claude)?;
    }

    println!("[claude9] box_id={}", box_id);
    println!("[claude9] next: claude9 task {} \"<prompt>\"", box_id);
    Ok(())
}

pub fn task(args: TaskArgs) -> Result<()> {
    let cfg = config::load()?;
    let prompt = resolve_prompt(join_opt(&args.prompt), args.file.as_deref())?
        .ok_or_else(|| anyhow!("no prompt given (positional args or -f FILE)"))?;
    run_claude_task(&args.box_id, &prompt, None, &cfg.claude)
}

pub fn resume(args: ResumeArgs) -> Result<()> {
    let cfg = config::load()?;
    let session = state::load_session(&args.box_id)?;
    let prompt = resolve_prompt(join_opt(&args.prompt), args.file.as_deref())?
        .ok_or_else(|| anyhow!("no follow-up given (positional args or -f FILE)"))?;
    run_claude_task(&args.box_id, &prompt, Some(&session), &cfg.claude)
}

fn join_opt(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn resolve_prompt(inline: Option<String>, file: Option<&Path>) -> Result<Option<String>> {
    if let Some(p) = inline {
        return Ok(Some(p));
    }
    if let Some(f) = file {
        let text = std::fs::read_to_string(f)
            .with_context(|| format!("reading prompt file {}", f.display()))?;
        return Ok(Some(text));
    }
    Ok(None)
}

/// Accumulates state across a `claude -p --output-format stream-json` run.
/// The closure handed to `box_exec_streaming` forwards each JSON line here;
/// we print progress live and remember the final session id + result for the
/// caller to persist after the exec finishes.
struct ClaudeStreamState {
    session_id: Option<String>,
    is_error: bool,
    final_result: Option<String>,
}

impl ClaudeStreamState {
    fn new() -> Self {
        Self {
            session_id: None,
            is_error: false,
            final_result: None,
        }
    }

    fn handle_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }

        let output = match ClaudeOutput::parse_json_tolerant(trimmed) {
            Ok(o) => o,
            Err(e) => {
                // Not a known claude event — surface the raw line so the
                // user can see what's happening even if we can't type it.
                eprintln!("[claude9] unparsed: {}", e.raw_line);
                return;
            }
        };

        if let Some(sid) = output.session_id() {
            if self.session_id.as_deref() != Some(sid) {
                self.session_id = Some(sid.to_string());
            }
        }

        match &output {
            ClaudeOutput::System(sys) if sys.is_init() => {
                println!("[claude9] session init");
            }
            ClaudeOutput::Assistant(_) => {
                if let Some(text) = output.text_content() {
                    println!("{}", text);
                }
                for tool in output.tool_uses() {
                    println!("[tool: {}]", tool.name);
                }
            }
            ClaudeOutput::Result(result) => {
                // Don't re-print result.result — it's the same text the
                // Assistant events already streamed. Just remember it for
                // the error path below.
                self.is_error = result.is_error;
                self.final_result = result.result.clone();
            }
            ClaudeOutput::Error(err) => {
                eprintln!("[claude9] anthropic error: {:?}", err);
            }
            _ => {} // User echo, rate-limit events, control msgs — silent.
        }
    }
}

fn run_claude_task(
    box_id: &str,
    prompt: &str,
    resume_session: Option<&str>,
    claude_opts: &ClaudeOptions,
) -> Result<()> {
    let mut env = HashMap::new();
    env.insert("C9_PROMPT".into(), prompt.to_string());

    // stream-json requires --verbose when used with --print/-p.
    // Default resume behavior reuses the same session_id; pass --fork-session
    // if you ever want a fresh id per turn.
    //
    // Extra flags (permission mode, tool allow/deny-lists) come from the
    // `[claude]` section of config.toml. Headless mode can't show approval
    // prompts, so tools like WebFetch are silently denied unless the user
    // opts in via `permission_mode = "bypassPermissions"` or an explicit
    // `allowed_tools` list.
    let extra_flags = build_claude_flags(claude_opts);
    let resume_frag = resume_session
        .map(|sid| format!(" --resume {}", shell_single_quote(sid)))
        .unwrap_or_default();

    let cmd = format!(
        r#"claude -p{}{} --output-format stream-json --verbose "$C9_PROMPT""#,
        resume_frag, extra_flags,
    );

    println!("[claude9] running claude -p on {} (stream)", box_id);

    let mut stream = ClaudeStreamState::new();
    run9::box_exec_streaming(
        box_id,
        REMOTE_USER,
        WORKSPACE,
        &env,
        &["/bin/bash", "-lc", &cmd],
        |line| stream.handle_line(line),
    )?;

    // Persist session id regardless of success, so a partially-failed turn
    // can still be resumed.
    if let Some(sid) = &stream.session_id {
        state::save_session(box_id, sid)?;
        eprintln!("[claude9] session: {}", sid);
    }

    if stream.is_error {
        bail!(
            "claude -p reported error: {}",
            stream.final_result.as_deref().unwrap_or("<no result>")
        );
    }

    Ok(())
}

fn wait_for_ready(box_id: &str, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        let view = run9::box_inspect(box_id)?;
        let state = view
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if state == "ready" {
            return Ok(());
        }
        if start.elapsed() > timeout {
            bail!(
                "box {} did not reach ready state within {:?}; last state={}",
                box_id,
                timeout,
                state
            );
        }
        thread::sleep(Duration::from_secs(3));
    }
}

fn extract_box_id(view: &Value) -> Result<String> {
    // `run9 box inspect` returns `box_id`; `run9 box create` may use a
    // different key, so keep `id` as a fallback.
    for key in ["box_id", "id"] {
        if let Some(s) = view.get(key).and_then(|v| v.as_str()) {
            return Ok(s.to_string());
        }
    }
    bail!("could not find box id in create response: {}", view)
}

/// Render a `ClaudeOptions` as extra CLI flags, ready to be spliced into
/// a bash command. Returns an empty string if no options are set; otherwise
/// each flag is prefixed with a leading space so concatenation stays clean.
/// Values are single-quoted so tool patterns like `Bash(git:*)` survive
/// bash parsing.
fn build_claude_flags(opts: &ClaudeOptions) -> String {
    let mut out = String::new();
    if let Some(mode) = opts.permission_mode.as_deref() {
        out.push_str(" --permission-mode ");
        out.push_str(&shell_single_quote(mode));
    }
    if opts.dangerously_skip_permissions {
        out.push_str(" --dangerously-skip-permissions");
    }
    if !opts.allowed_tools.is_empty() {
        out.push_str(" --allowedTools ");
        out.push_str(&shell_single_quote(&opts.allowed_tools.join(",")));
    }
    if !opts.disallowed_tools.is_empty() {
        out.push_str(" --disallowedTools ");
        out.push_str(&shell_single_quote(&opts.disallowed_tools.join(",")));
    }
    out
}

/// Wrap a string in single quotes for safe interpolation into a bash
/// command. Any embedded `'` is escaped as `'\''` — the standard trick
/// to close, escape-literal, and reopen the quote.
fn shell_single_quote(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}
