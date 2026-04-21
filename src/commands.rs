use anyhow::{anyhow, bail, Context, Result};
use chrono::{Local, Utc};
use claude_codes::{ClaudeOutput, ContentBlock, ToolResultBlock, ToolResultContent, ToolUseBlock};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::{BashArgs, JoinArgs, ResumeArgs, SpawnArgs, StopArgs, TalkArgs, TaskArgs};
use crate::config::{self, ClaudeOptions, REMOTE_USER, REPOS_DIR, WORKSPACE};
use crate::resolver;
use crate::run9;
use crate::state::{self, BgTask, BoxMeta};

/// Width of the `[HH:MM:SS] ` prefix that `elog` emits — used by
/// `elog_cont` to left-pad continuation lines so they visually align
/// with the first line of the same event.
const TS_PAD: &str = "           "; // 11 spaces == "[HH:MM:SS] ".len()

/// Timestamped log line on stderr. Everything that's not the assistant's
/// result text goes through here, so the user can pipe stdout cleanly.
fn elog(msg: impl AsRef<str>) {
    eprintln!("[{}] {}", Local::now().format("%H:%M:%S"), msg.as_ref());
}

/// Continuation line for a multi-line event (tool result body, git
/// output, etc.). Skips the timestamp but keeps the same indentation,
/// so a block reads as one unit instead of N separate events.
fn elog_cont(msg: impl AsRef<str>) {
    eprintln!("{}{}", TS_PAD, msg.as_ref());
}

/// Emit a block of text to stderr with one timestamp on the first
/// non-empty line. Used for subprocess passthrough (git fetch, repo
/// clone output) so a burst of lines reads as a single event.
fn elog_lines(text: &str) {
    let mut first = true;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if first {
            elog(line);
            first = false;
        } else {
            elog_cont(line);
        }
    }
}

pub fn config_cmd() -> Result<()> {
    let path = config::ensure_exists()?;
    println!("{}", path.display());
    if let Ok(editor) = std::env::var("EDITOR") {
        let _ = std::process::Command::new(editor).arg(&path).status();
    }
    Ok(())
}

pub fn spawn(args: SpawnArgs) -> Result<String> {
    let cfg = config::load()?;
    let base_box = args
        .base_box
        .clone()
        .unwrap_or_else(|| cfg.defaults.base_box.clone());
    let shape = args
        .shape
        .clone()
        .unwrap_or_else(|| cfg.defaults.shape.clone());

    elog(format!(
        "[claude9] resolving base snap from box '{base_box}'"
    ));
    let snap_id = resolver::resolve_base_snap(&base_box)?;
    elog(format!("[claude9] base snap: {snap_id}"));

    let whoami = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let mut labels: Vec<(&str, &str)> = vec![
        ("claude9", "managed"),
        ("claude9-base", base_box.as_str()),
        ("claude9-owner", whoami.as_str()),
    ];
    if let Some(desc) = args.desc.as_deref() {
        labels.push(("claude9-task", desc));
    }

    // When --name is given it's a prefix; append 8 random hex chars so
    // multiple users (or multiple spawns by the same user) in the shared
    // run9 org never collide on the box name.
    let box_name = args
        .name
        .as_deref()
        .map(|prefix| format!("{}-{}", prefix, random_hex(4)));

    elog(format!("[claude9] creating box (shape={shape})"));
    let created = run9::box_create_from_snap(box_name.as_deref(), &snap_id, &shape, &labels)?;
    let box_id = extract_box_id(&created)?;
    elog(format!("[claude9] box created: {box_id}"));

    wait_for_ready(&box_id, Duration::from_secs(180))?;
    elog("[claude9] box ready");

    // Ensure repos dir exists (cheap idempotent op in case base snap lacks it).
    run9::box_exec(
        &box_id,
        REMOTE_USER,
        WORKSPACE,
        &HashMap::new(),
        &["/bin/sh", "-lc", &format!("mkdir -p {REPOS_DIR}")],
    )?;

    // Sync repos (serial; v1 doesn't do parallel).
    let mut project_names: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    if args.no_update {
        elog("[claude9] --no-update set, skipping repo sync");
        for p in &cfg.projects {
            project_names.push(p.local_name());
        }
    } else {
        for p in &cfg.projects {
            let local = p.local_name();
            project_names.push(local.clone());
            elog(format!("[claude9] sync {} -> {}", p.repo, local));

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
                    elog_lines(res.stdout.trim_end());
                    elog_lines(res.stderr.trim_end());
                }
                Err(e) => {
                    elog(format!("[claude9] sync failed for {}: {}", p.repo, e));
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
        elog(format!("[claude9] repo sync failures: {failed:?}"));
    }

    // Print box_id early — before the optional task streams its output —
    // so the user (or a wrapping script) can capture it regardless of how
    // much claude output follows.
    elog(format!("[claude9] box_id={box_id}"));

    // Optional inline task.
    let prompt = resolve_prompt(args.task.clone(), args.task_file.as_deref())?;
    if let Some(ref p) = prompt {
        run_claude_task(&box_id, p, None, &cfg.claude)?;
    }

    let next_cmd = if prompt.is_some() { "resume" } else { "task" };
    elog(format!(
        "[claude9] next: claude9 {next_cmd} {box_id} \"<prompt>\""
    ));
    Ok(box_id)
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

/// Transparent passthrough to `run9 box exec -it ... -- /bin/bash`.
/// Target box defaults to `defaults.base_box` from config.toml — the
/// main use case is hand-preparing the base box per the "Base box
/// contract" section of SKILL.md. `user` and `workdir` are fixed to
/// `guy` and `/home/guy/workspace`: matches the remote layout claude9
/// already assumes everywhere else, and there's no reason to operate on
/// any other path via this shortcut.
///
/// Positional `-- ARGS...` get forwarded to bash as its own args, so
/// `claude9 bash -- -lc 'echo hi'` runs a one-shot non-interactive
/// command while bare `claude9 bash` drops into an interactive shell.
pub fn bash(args: BashArgs) -> Result<()> {
    let cfg = config::load()?;
    let target = args
        .box_name
        .clone()
        .unwrap_or_else(|| cfg.defaults.base_box.clone());

    let mut cmd: Vec<String> = vec!["/bin/bash".into()];
    cmd.extend(args.bash_args.iter().cloned());
    let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();

    elog(format!(
        "[claude9] bash -> {target} (user={REMOTE_USER}, workdir={WORKSPACE})"
    ));
    let status = run9::box_exec_interactive(&target, REMOTE_USER, WORKSPACE, &cmd_refs)?;
    if !status.success() {
        bail!(
            "bash exited non-zero (code {})",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

pub fn join(args: JoinArgs) -> Result<()> {
    let task = state::load_bg_task(&args.box_id)?
        .ok_or_else(|| anyhow!("no background task on box {}", args.box_id))?;
    elog(format!(
        "[claude9] joining task on {} (exec_id={})",
        args.box_id, task.exec_id
    ));
    poll_bg_task(&args.box_id, &task.exec_id, true)
}

pub fn stop(args: StopArgs) -> Result<()> {
    let task = state::load_bg_task(&args.box_id)?
        .ok_or_else(|| anyhow!("no background task on box {}", args.box_id))?;
    elog(format!("[claude9] stopping task on {}", args.box_id));
    match run9::box_exec_bg_kill(&task.exec_id) {
        Ok(()) => elog("[claude9] stopped"),
        Err(e) => elog(format!("[claude9] kill returned: {e}")),
    }
    state::clear_bg_task(&args.box_id)?;
    Ok(())
}

pub fn ps() -> Result<()> {
    let tasks = state::list_bg_tasks()?;
    if tasks.is_empty() {
        elog("[claude9] no background tasks");
        return Ok(());
    }
    for (box_id, t) in &tasks {
        let age = Utc::now().signed_duration_since(t.started_at);
        let age_str = if age.num_hours() > 0 {
            format!("{}h{}m ago", age.num_hours(), age.num_minutes() % 60)
        } else {
            format!("{}m ago", age.num_minutes())
        };
        let snippet: String = t.prompt_snippet.chars().take(60).collect();
        eprintln!("  {box_id}  {age_str:>10}  {snippet}");
    }
    Ok(())
}

pub fn talk(args: TalkArgs) -> Result<()> {
    let mut cfg = config::load()?;
    // Per-invocation CLI overrides for model / effort, same pattern as
    // spawn's --base-box / --shape.
    if args.model.is_some() {
        cfg.claude.model = args.model.clone();
    }
    if args.effort.is_some() {
        cfg.claude.effort = args.effort.clone();
    }

    let first_prompt =
        resolve_prompt(args.first_prompt.clone(), args.first_prompt_file.as_deref())?;

    let box_id = match args.name.as_deref() {
        Some(prefix) => pick_or_spawn_box(prefix, args.desc.as_deref(), args.shape.as_deref())?,
        // No --name: always spawn fresh, let run9 auto-allocate the name
        // (same behavior as `claude9 spawn` with no --name).
        None => {
            elog("[claude9] no --name given; spawning a fresh auto-named box");
            spawn(SpawnArgs {
                name: None,
                desc: args.desc.clone(),
                task: None,
                task_file: None,
                no_update: false,
                base_box: None,
                shape: args.shape.clone(),
            })?
        }
    };

    run_claude_interactive(&box_id, first_prompt.as_deref(), &cfg.claude)
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
    /// `tool_use_id` → rendered label (e.g. `[Read] /path/to/file`).
    /// Populated when we see an Assistant `ToolUseBlock`; consumed when
    /// the matching `ToolResultBlock` arrives. Required because claude
    /// can fan out multiple tool calls in one turn, and results come back
    /// in a separate (often interleaved) User event — without this map
    /// the user can't tell which `↳` belongs to which call.
    tool_labels: HashMap<String, String>,
}

impl ClaudeStreamState {
    fn new() -> Self {
        Self {
            session_id: None,
            is_error: false,
            final_result: None,
            tool_labels: HashMap::new(),
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
                elog(format!("[claude9] unparsed: {}", e.raw_line));
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
                elog("[claude9] session init");
            }
            ClaudeOutput::Assistant(_) => {
                // Assistant text content is the only thing that reaches
                // stdout — keeps `claude9 task ... > out.md` clean.
                if let Some(text) = output.text_content() {
                    println!("{text}");
                }
                for tool in output.tool_uses() {
                    let lines = render_tool_use(tool);
                    // First line is the header (gets the timestamp);
                    // body lines use elog_cont so a multi-line Bash
                    // command / Edit diff reads as one event. Only the
                    // header is stored as the correlation label so the
                    // matching ToolResult echoes a compact back-ref
                    // instead of the full body.
                    let header = lines
                        .first()
                        .cloned()
                        .unwrap_or_else(|| format!("[{}]", tool.name));
                    for (i, line) in lines.iter().enumerate() {
                        if i == 0 {
                            elog(line);
                        } else {
                            elog_cont(line);
                        }
                    }
                    self.tool_labels.insert(tool.id.clone(), header);
                }
            }
            ClaudeOutput::User(user) => {
                // Tool results come back as synthetic user messages with
                // ToolResult content blocks. The real user prompt echo
                // shows up as Text blocks and is naturally skipped below.
                for block in &user.message.content {
                    if let ContentBlock::ToolResult(tr) = block {
                        let label = self.tool_labels.remove(&tr.tool_use_id);
                        render_tool_result(tr, label.as_deref());
                    }
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
                elog(format!("[claude9] anthropic error: {err:?}"));
            }
            _ => {} // Rate-limit events, control msgs — silent.
        }
    }
}

const BG_DEADLINE: &str = "10h";
const BG_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Max consecutive `pull-output` errors before we give up. Transient
/// errors happen right after `exec-bg` returns (backend not yet ready)
/// and sometimes once the exec is reaped after completion — both are
/// expected and resolved on retry.
const PULL_ERROR_LIMIT: u32 = 10;

fn ensure_no_active_bg_task(box_id: &str) -> Result<()> {
    let existing = state::load_bg_task(box_id)?;
    let task = match existing {
        Some(t) => t,
        None => return Ok(()),
    };
    // Probe whether the remote task is still alive.
    match run9::box_exec_bg_pull(&task.exec_id, false) {
        Ok(_) => {
            bail!(
                "box {box_id} already has a running task (exec_id={}). \
                 Stop it first: claude9 stop {box_id}",
                task.exec_id
            );
        }
        Err(_) => {
            state::clear_bg_task(box_id)?;
            Ok(())
        }
    }
}

fn run_claude_task(
    box_id: &str,
    prompt: &str,
    resume_session: Option<&str>,
    claude_opts: &ClaudeOptions,
) -> Result<()> {
    ensure_no_active_bg_task(box_id)?;

    let mut env = HashMap::new();
    env.insert("C9_PROMPT".into(), prompt.to_string());

    let extra_flags = build_claude_flags(claude_opts);
    let resume_frag = resume_session
        .map(|sid| format!(" --resume {}", shell_single_quote(sid)))
        .unwrap_or_default();

    let cmd = format!(
        r#"claude -p{resume_frag}{extra_flags} --output-format stream-json --verbose "$C9_PROMPT""#,
    );

    elog(format!("[claude9] launching background task on {box_id}"));

    let created = run9::box_exec_bg(
        box_id,
        REMOTE_USER,
        WORKSPACE,
        BG_DEADLINE,
        &env,
        &["/bin/bash", "-lc", &cmd],
    )?;

    let exec_id = created
        .get("exec_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("exec-bg response missing exec_id: {created}"))?
        .to_string();

    elog(format!("[claude9] exec_id={exec_id}"));

    let snippet: String = prompt.chars().take(200).collect();
    let bg = BgTask {
        exec_id: exec_id.clone(),
        started_at: Utc::now(),
        prompt_snippet: snippet,
    };
    state::save_bg_task(box_id, &bg)?;

    let kind = if resume_session.is_some() {
        "resume"
    } else {
        "task"
    };
    if let Err(e) = state::append_history(box_id, kind, prompt, None) {
        elog(format!("[claude9] warn: history write failed: {e}"));
    }

    poll_bg_task(box_id, &exec_id, false)
}

fn poll_bg_task(box_id: &str, exec_id: &str, from_start: bool) -> Result<()> {
    let mut stream = ClaudeStreamState::new();
    let mut line_buf = String::new();
    let mut session_saved = false;
    let mut first_pull = true;
    let mut consecutive_errors: u32 = 0;

    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = interrupted.clone();
        ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
        })
        .ok();
    }

    loop {
        if interrupted.load(Ordering::SeqCst) {
            elog(format!(
                "[claude9] detached — task keeps running. Rejoin: claude9 join {box_id}"
            ));
            return Ok(());
        }

        let use_from_start = from_start && first_pull;
        first_pull = false;

        match run9::box_exec_bg_pull(exec_id, use_from_start) {
            Ok(chunk) => {
                consecutive_errors = 0;
                if !chunk.is_empty() {
                    line_buf.push_str(&chunk);
                    while let Some(pos) = line_buf.find('\n') {
                        let line = line_buf[..pos].to_string();
                        line_buf = line_buf[pos + 1..].to_string();
                        stream.handle_line(&line);
                    }
                }
            }
            Err(e) => {
                // Transient errors happen briefly right after exec-bg
                // returns (backend not ready yet) and sometimes once the
                // exec is cleaned up. Retry up to PULL_ERROR_LIMIT before
                // giving up.
                consecutive_errors += 1;
                if stream.final_result.is_some() || consecutive_errors >= PULL_ERROR_LIMIT {
                    if stream.final_result.is_none() {
                        elog(format!(
                            "[claude9] pull-output failed {PULL_ERROR_LIMIT}× in a row: {e}"
                        ));
                    }
                    break;
                }
            }
        }

        if !session_saved {
            if let Some(sid) = &stream.session_id {
                if let Err(e) = state::save_session(box_id, sid) {
                    elog(format!("[claude9] warn: session write failed: {e}"));
                } else {
                    elog(format!("[claude9] session: {sid}"));
                }
                session_saved = true;
            }
        }

        if stream.final_result.is_some() {
            break;
        }

        thread::sleep(BG_POLL_INTERVAL);
    }

    // Flush any remaining partial line.
    let tail = line_buf.trim();
    if !tail.is_empty() {
        stream.handle_line(tail);
    }

    if !session_saved {
        if let Some(sid) = &stream.session_id {
            let _ = state::save_session(box_id, sid);
            elog(format!("[claude9] session: {sid}"));
        }
    }

    // Only clear the local bg.toml when the task actually completed —
    // i.e. we saw `final_result` in the stream. Hitting `PULL_ERROR_LIMIT`
    // without a final_result is ambiguous: could be a long network
    // outage with the remote task still running. Keep the record so the
    // user can retry `claude9 join` or discard via `claude9 stop`.
    if stream.final_result.is_some() {
        state::clear_bg_task(box_id)?;
    } else {
        elog(format!(
            "[claude9] detached — task may still be running remotely. \
             Rejoin: claude9 join {box_id}  |  Discard: claude9 stop {box_id}"
        ));
    }

    if stream.is_error {
        bail!(
            "claude -p reported error: {}",
            stream.final_result.as_deref().unwrap_or("<no result>")
        );
    }

    Ok(())
}

/// Hand a TTY over to an interactive `claude` inside the box. We never
/// intercept stdout/stderr here — `box_exec_interactive` wires inherit
/// everywhere so the terminal experience matches running `claude`
/// locally. `--resume` is intentionally *not* passed: interactive mode
/// starts a fresh session. The seed prompt is passed as `$0` to bash so
/// no shell-escaping is needed on the caller side.
fn run_claude_interactive(
    box_id: &str,
    first_prompt: Option<&str>,
    claude_opts: &ClaudeOptions,
) -> Result<()> {
    let flags = build_claude_flags(claude_opts);

    // `claude <flags> "$0"` — $0 is the seed prompt (or empty if none).
    // When empty, claude sees no positional arg and launches into the
    // normal interactive REPL.
    let inner = format!(r#"claude{flags} "$0""#);
    let seed = first_prompt.unwrap_or("");

    elog(format!("[claude9] talk -> {box_id}"));
    if let Err(e) = state::append_history(box_id, "talk", seed, None) {
        elog(format!("[claude9] warn: history write failed: {e}"));
    }

    let status = run9::box_exec_interactive(
        box_id,
        REMOTE_USER,
        WORKSPACE,
        &["/bin/bash", "-lc", &inner, seed],
    )?;
    if !status.success() {
        bail!(
            "interactive session exited non-zero (code {})",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

/// Look up boxes whose id starts with `<prefix>-` under `.claude9/state/`.
/// - 0 matches → spawn a fresh box with this prefix (same as `claude9 spawn --name`).
/// - 1 match → use it.
/// - N matches → list them with created_at + last activity + last prompt
///   snippet, prompt the user for a 1-based index on stdin.
fn pick_or_spawn_box(prefix: &str, desc: Option<&str>, shape: Option<&str>) -> Result<String> {
    let ids = state::list_box_ids_by_prefix(prefix)?;

    if ids.is_empty() {
        elog(format!(
            "[claude9] no box matches prefix '{prefix}-*'; spawning a new one"
        ));
        return spawn_for_interactive(prefix, desc, shape);
    }

    // Warn if the user passed --shape but we're reusing an existing
    // box. Can't resize run9 boxes in place, so the flag is silently
    // no-op otherwise — better to surface it.
    if shape.is_some() {
        elog(
            "[claude9] --shape ignored: reusing an existing box, which cannot \
             be resized. Pass a different --name (or none) to spawn fresh.",
        );
    }

    // Gather metadata for sorting / display.
    let mut infos: Vec<BoxPickInfo> = ids.into_iter().map(|id| BoxPickInfo::load(&id)).collect();
    // Newest-first by created_at; unknown dates sort last.
    infos.sort_by_key(|i| std::cmp::Reverse(i.sort_key()));

    if infos.len() == 1 {
        let id = infos.into_iter().next().unwrap().box_id;
        elog(format!("[claude9] reusing box {id}"));
        return Ok(id);
    }

    elog(format!(
        "[claude9] {} boxes match prefix '{}-*':",
        infos.len(),
        prefix
    ));
    for (i, info) in infos.iter().enumerate() {
        elog(format!("  [{}] {}", i + 1, info.display_line()));
    }
    let n = infos.len();
    let choice = prompt_index_stdin(n)?;
    Ok(infos.into_iter().nth(choice - 1).unwrap().box_id)
}

/// Summary row for the interactive picker. `created_at` comes from
/// `meta.toml`; `last_activity` / `last_prompt` come from the newest
/// `history.jsonl` entry. Any of these can be missing (old box dirs
/// without meta, boxes that were never `task`'d).
struct BoxPickInfo {
    box_id: String,
    created_at: Option<chrono::DateTime<Utc>>,
    last_activity: Option<chrono::DateTime<Utc>>,
    last_kind: Option<String>,
    last_prompt: Option<String>,
}

impl BoxPickInfo {
    fn load(box_id: &str) -> Self {
        let meta = state::load_meta(box_id).ok();
        let history = state::load_history(box_id).unwrap_or_default();
        let last = history.last().cloned();
        BoxPickInfo {
            box_id: box_id.to_string(),
            created_at: meta.map(|m| m.created_at),
            last_activity: last.as_ref().map(|e| e.ts),
            last_kind: last.as_ref().map(|e| e.kind.clone()),
            last_prompt: last.as_ref().map(|e| e.prompt_snippet.clone()),
        }
    }

    fn sort_key(&self) -> chrono::DateTime<Utc> {
        // Prefer last activity; fall back to creation time; fall back
        // to epoch so unknown rows bubble to the bottom.
        self.last_activity
            .or(self.created_at)
            .unwrap_or_else(|| chrono::DateTime::<Utc>::from_timestamp(0, 0).unwrap())
    }

    fn display_line(&self) -> String {
        let created = self
            .created_at
            .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "?".into());
        let last = match (self.last_activity, self.last_kind.as_deref()) {
            (Some(t), Some(kind)) => format!(
                "last {} @ {}",
                kind,
                t.with_timezone(&Local).format("%Y-%m-%d %H:%M")
            ),
            _ => "no activity".into(),
        };
        let prompt = self
            .last_prompt
            .as_deref()
            .map(|p| truncate(&p.replace('\n', " "), 60))
            .unwrap_or_default();
        if prompt.is_empty() {
            format!("{}  (created {}, {})", self.box_id, created, last)
        } else {
            format!(
                "{}  (created {}, {})\n             ↳ {}",
                self.box_id, created, last, prompt
            )
        }
    }
}

/// Read a 1-based index from stdin. Loops until the user enters a valid
/// number in `[1, n]` — blank / non-numeric / out-of-range re-prompts
/// rather than crashing the command.
fn prompt_index_stdin(n: usize) -> Result<usize> {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    loop {
        eprint!("[claude9] pick [1-{n}]: ");
        stdout.flush().ok();
        let mut line = String::new();
        stdin.lock().read_line(&mut line).context("reading stdin")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed.parse::<usize>() {
            Ok(i) if (1..=n).contains(&i) => return Ok(i),
            _ => {
                elog(format!("[claude9] '{trimmed}' is not in 1..={n}"));
            }
        }
    }
}

/// Spawn a fresh box for `claude9 talk` when no existing one matches
/// the prefix. Mirrors `spawn()` but skips the optional task hook —
/// the talk session is the task.
fn spawn_for_interactive(prefix: &str, desc: Option<&str>, shape: Option<&str>) -> Result<String> {
    spawn(SpawnArgs {
        name: Some(prefix.to_string()),
        desc: desc.map(|s| s.to_string()),
        task: None,
        task_file: None,
        no_update: false,
        base_box: None,
        shape: shape.map(|s| s.to_string()),
    })
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
            bail!("box {box_id} did not reach ready state within {timeout:?}; last state={state}");
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
    bail!("could not find box id in create response: {view}")
}

/// Render a `ClaudeOptions` as extra CLI flags, ready to be spliced into
/// a bash command. Returns an empty string if no options are set; otherwise
/// each flag is prefixed with a leading space so concatenation stays clean.
/// Values are single-quoted so tool patterns like `Bash(git:*)` survive
/// bash parsing.
fn build_claude_flags(opts: &ClaudeOptions) -> String {
    let mut out = String::new();
    if let Some(model) = opts.model.as_deref() {
        out.push_str(" --model ");
        out.push_str(&shell_single_quote(model));
    }
    if let Some(effort) = opts.effort.as_deref() {
        out.push_str(" --effort ");
        out.push_str(&shell_single_quote(effort));
    }
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
    format!("'{escaped}'")
}

/// Return `n` random bytes formatted as 2*n lowercase hex chars.
/// Reads from `/dev/urandom` — no extra deps needed.
fn random_hex(n: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

// ────────────────────────────────────────────────────────────────────────
//  Tool rendering
//
//  claude -p streams tool calls as Assistant events with ToolUseBlock
//  content, and tool results come back as User events with ToolResult
//  blocks. We render a one-line summary for each, choosing which field
//  to show based on the tool name. Unknown tools fall back to a
//  truncated JSON dump.
// ────────────────────────────────────────────────────────────────────────

/// Max chars for tool input summaries (e.g. long Bash commands).
const TOOL_INPUT_MAX: usize = 240;
/// Max number of lines to preview from a tool result. Anything past
/// this is replaced with a `… (N more lines)` footer.
const TOOL_RESULT_MAX_LINES: usize = 8;
/// Max chars per preview line. Long lines get an ellipsis so we don't
/// blow out the terminal on a single run of concatenated output.
const TOOL_RESULT_LINE_MAX: usize = 200;

/// Render a tool call as one or more display lines. Element `[0]` is
/// the header (gets a timestamp via `elog`); any extra elements are
/// body continuation lines (`elog_cont`, no timestamp). Output is
/// capped at `TOOL_RESULT_MAX_LINES` with a `… (N more lines)` footer.
///
/// Multi-line inputs (Bash heredocs, Edit old/new strings, Write
/// content, Task prompts) get expanded here — a single-line summary
/// for a 40-line heredoc drops all the interesting detail.
fn render_tool_use(tool: &ToolUseBlock) -> Vec<String> {
    let name = tool.name.as_str();
    let input = &tool.input;
    let s = |k: &str| input.get(k).and_then(|v| v.as_str());
    let u = |k: &str| input.get(k).and_then(|v| v.as_u64());
    let b = |k: &str| input.get(k).and_then(|v| v.as_bool());

    let mut out: Vec<String> = Vec::new();

    match name {
        "Bash" => {
            let cmd = s("command").unwrap_or("");
            let cmd_lines: Vec<&str> = cmd.lines().collect();
            let first = cmd_lines.first().copied().unwrap_or("");
            let mut header = format!("[Bash] {}", truncate(first, TOOL_INPUT_MAX));
            if let Some(desc) = s("description") {
                header.push_str(&format!("  # {}", truncate(desc, 80)));
            }
            out.push(header);
            for line in cmd_lines.iter().skip(1) {
                out.push(format!("       {}", truncate(line, TOOL_RESULT_LINE_MAX)));
            }
        }
        "Read" => {
            let path = s("file_path").unwrap_or("?");
            let header = match (u("offset"), u("limit")) {
                (Some(o), Some(l)) => format!("[Read] {} (lines {}–{})", path, o, o + l),
                (Some(o), None) => format!("[Read] {path} (from line {o})"),
                _ => format!("[Read] {path}"),
            };
            out.push(header);
        }
        "Write" => {
            let path = s("file_path").unwrap_or("?");
            let content = s("content").unwrap_or("");
            let bytes = content.len();
            let nlines = content.lines().count();
            out.push(format!("[Write] {path} ({bytes} bytes, {nlines} lines)"));
            for line in content.lines() {
                out.push(format!("       {}", truncate(line, TOOL_RESULT_LINE_MAX)));
            }
        }
        "Edit" => {
            let path = s("file_path").unwrap_or("?");
            let old = s("old_string").unwrap_or("");
            let new = s("new_string").unwrap_or("");
            let header = if b("replace_all").unwrap_or(false) {
                format!("[Edit] {path} (replace_all)")
            } else {
                format!("[Edit] {path}")
            };
            out.push(header);
            // Unified-diff-ish preview: `- old …` then `+ new …`. Keeps
            // the order meaningful even when one side is multi-line.
            for line in old.lines() {
                out.push(format!("     - {}", truncate(line, TOOL_RESULT_LINE_MAX)));
            }
            for line in new.lines() {
                out.push(format!("     + {}", truncate(line, TOOL_RESULT_LINE_MAX)));
            }
        }
        "Grep" => {
            let pat = s("pattern").unwrap_or("");
            let mut header = format!("[Grep] /{}/", truncate(pat, 80));
            if let Some(p) = s("path") {
                header.push_str(&format!(" in {p}"));
            }
            if let Some(g) = s("glob") {
                header.push_str(&format!(" glob={g}"));
            }
            if let Some(t) = s("type") {
                header.push_str(&format!(" type={t}"));
            }
            out.push(header);
        }
        "Glob" => {
            let pat = s("pattern").unwrap_or("");
            let mut header = format!("[Glob] {pat}");
            if let Some(p) = s("path") {
                header.push_str(&format!(" in {p}"));
            }
            out.push(header);
        }
        "WebFetch" => {
            let url = s("url").unwrap_or("?");
            out.push(format!("[WebFetch] {url}"));
            if let Some(prompt) = s("prompt") {
                for line in prompt.lines() {
                    out.push(format!("       {}", truncate(line, TOOL_RESULT_LINE_MAX)));
                }
            }
        }
        "WebSearch" => {
            let query = s("query").unwrap_or("?");
            out.push(format!("[WebSearch] {}", truncate(query, TOOL_INPUT_MAX)));
        }
        "Task" => {
            let sub = s("subagent_type").unwrap_or("?");
            let desc = s("description").unwrap_or("");
            out.push(format!("[Task:{}] {}", sub, truncate(desc, 120)));
            if let Some(prompt) = s("prompt") {
                for line in prompt.lines() {
                    out.push(format!("       {}", truncate(line, TOOL_RESULT_LINE_MAX)));
                }
            }
        }
        "TodoWrite" => {
            let todos = input.get("todos").and_then(|v| v.as_array());
            let n = todos.map(|a| a.len()).unwrap_or(0);
            out.push(format!("[TodoWrite] {n} item(s)"));
            if let Some(items) = todos {
                for item in items {
                    let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    let marker = match status {
                        "completed" => "✓",
                        "in_progress" => "→",
                        _ => "·",
                    };
                    out.push(format!(
                        "     {} {}",
                        marker,
                        truncate(content, TOOL_RESULT_LINE_MAX)
                    ));
                }
            }
        }
        _ => {
            // Unknown tool — dump compact JSON of input so nothing is lost.
            let json = serde_json::to_string(&tool.input).unwrap_or_default();
            out.push(format!("[{}] {}", name, truncate(&json, TOOL_INPUT_MAX)));
        }
    }

    // Uniform line cap — shared with render_tool_result. A 120-line
    // Bash heredoc would otherwise bury everything else in the stream.
    cap_preview_lines(&mut out);
    out
}

/// Clip `lines` in-place to at most `TOOL_RESULT_MAX_LINES` entries,
/// appending a `… (N more lines)` footer when anything was dropped.
/// Callers never need to apply the cap themselves. The footer is
/// indented to match the standard body indent used by every renderer.
fn cap_preview_lines(lines: &mut Vec<String>) {
    if lines.len() <= TOOL_RESULT_MAX_LINES {
        return;
    }
    let dropped = lines.len() - TOOL_RESULT_MAX_LINES;
    lines.truncate(TOOL_RESULT_MAX_LINES);
    lines.push(format!("       … ({dropped} more lines)"));
}

/// Emit a multi-line preview of a tool result to stderr. Newlines are
/// preserved so file reads / build output stay readable. The header line
/// gets a `↳` marker (plus `✗` on error) and echoes the original tool
/// call's label (e.g. `↳ [Read] file.rs`) so concurrent fan-out calls
/// stay visually paired with their results. Continuation lines are
/// indented to align under the header. Everything past
/// `TOOL_RESULT_MAX_LINES` collapses into a `… (N more lines)` footer.
/// Structured content is flattened by concatenating any `text` fields.
fn render_tool_result(tr: &ToolResultBlock, call_label: Option<&str>) {
    let raw = match tr.content.as_ref() {
        Some(ToolResultContent::Text(t)) => t.clone(),
        Some(ToolResultContent::Structured(blocks)) => blocks
            .iter()
            .filter_map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n"),
        None => return,
    };
    if raw.trim().is_empty() {
        return;
    }

    // Drop empty / whitespace-only lines and trim trailing whitespace
    // so `  123  \t` doesn't waste horizontal space.
    let lines: Vec<String> = raw
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .map(|l| truncate(l, TOOL_RESULT_LINE_MAX))
        .collect();

    if lines.is_empty() {
        return;
    }

    let total = lines.len();
    let show = total.min(TOOL_RESULT_MAX_LINES);
    let marker = if tr.is_error.unwrap_or(false) {
        "✗ "
    } else {
        ""
    };

    // Header: correlate back to the call. If we somehow missed the
    // tool_use (shouldn't happen in practice, but stream-json is
    // best-effort), fall back to a short id suffix so there's still
    // *some* anchor for the reader.
    let header = match call_label {
        Some(l) => format!("  ↳ {marker}{l}"),
        None => format!("  ↳ {}<unknown call {}>", marker, short_id(&tr.tool_use_id)),
    };
    elog(header);

    for line in lines.iter().take(show) {
        elog_cont(format!("    {line}"));
    }

    if total > show {
        elog_cont(format!("    … ({} more lines)", total - show));
    }
}

/// Last 6 chars of a tool_use_id like `toolu_01ABC...xyz` — enough to
/// disambiguate within one turn without flooding the log with noise.
fn short_id(id: &str) -> String {
    let n = id.chars().count();
    if n <= 6 {
        return id.to_string();
    }
    id.chars().skip(n - 6).collect()
}

/// Truncate `s` to at most `max` chars, appending `…` when truncated.
/// Char-count aware so it doesn't slice UTF-8 mid-codepoint.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shell_single_quote_simple() {
        assert_eq!(shell_single_quote("hello"), "'hello'");
        assert_eq!(shell_single_quote(""), "''");
    }

    #[test]
    fn shell_single_quote_embedded_quote() {
        // The close-escape-reopen trick: `it's` → `'it'\''s'`
        assert_eq!(shell_single_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn shell_single_quote_tool_pattern() {
        // Real use case: `Bash(git:*)` must survive splicing into a
        // `bash -lc '...'` invocation.
        assert_eq!(shell_single_quote("Bash(git:*)"), "'Bash(git:*)'");
    }

    #[test]
    fn truncate_under_and_at_limit() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("", 10), "");
        assert_eq!(truncate("exact", 5), "exact");
    }

    #[test]
    fn truncate_over_limit() {
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn truncate_respects_utf8_boundaries() {
        // max is char-count, not byte count — slicing mid-codepoint
        // would panic, so this would catch that regression.
        assert_eq!(truncate("héllo world", 5), "héllo…");
        assert_eq!(truncate("中文字符串测试", 4), "中文字符…");
    }

    #[test]
    fn short_id_returns_last_six() {
        assert_eq!(short_id("toolu_01ABCDEFxyz"), "DEFxyz");
    }

    #[test]
    fn short_id_returns_short_unchanged() {
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id("abcdef"), "abcdef");
    }

    #[test]
    fn build_claude_flags_empty() {
        let opts = ClaudeOptions::default();
        assert_eq!(build_claude_flags(&opts), "");
    }

    #[test]
    fn build_claude_flags_model_and_effort() {
        let opts = ClaudeOptions {
            model: Some("opus".into()),
            effort: Some("max".into()),
            ..ClaudeOptions::default()
        };
        assert_eq!(build_claude_flags(&opts), " --model 'opus' --effort 'max'");
    }

    #[test]
    fn build_claude_flags_permission_and_skip() {
        let opts = ClaudeOptions {
            permission_mode: Some("bypassPermissions".into()),
            dangerously_skip_permissions: true,
            ..ClaudeOptions::default()
        };
        assert_eq!(
            build_claude_flags(&opts),
            " --permission-mode 'bypassPermissions' --dangerously-skip-permissions"
        );
    }

    #[test]
    fn build_claude_flags_tool_lists_joined_and_quoted() {
        let opts = ClaudeOptions {
            allowed_tools: vec!["WebFetch".into(), "Bash(git:*)".into()],
            disallowed_tools: vec!["WebSearch".into()],
            ..ClaudeOptions::default()
        };
        assert_eq!(
            build_claude_flags(&opts),
            " --allowedTools 'WebFetch,Bash(git:*)' --disallowedTools 'WebSearch'"
        );
    }

    #[test]
    fn extract_box_id_prefers_box_id_over_id() {
        let view = json!({ "box_id": "db9-abc", "id": "other" });
        assert_eq!(extract_box_id(&view).unwrap(), "db9-abc");
    }

    #[test]
    fn extract_box_id_falls_back_to_id() {
        let view = json!({ "id": "fallback" });
        assert_eq!(extract_box_id(&view).unwrap(), "fallback");
    }

    #[test]
    fn extract_box_id_errors_when_missing() {
        let view = json!({ "something_else": 1 });
        assert!(extract_box_id(&view).is_err());
    }

    #[test]
    fn cap_preview_lines_keeps_short_input() {
        let mut lines: Vec<String> = (0..5).map(|i| format!("line {i}")).collect();
        cap_preview_lines(&mut lines);
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn cap_preview_lines_truncates_long_input() {
        let mut lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        cap_preview_lines(&mut lines);
        assert_eq!(lines.len(), TOOL_RESULT_MAX_LINES + 1);
        let footer = lines.last().unwrap();
        assert!(footer.contains(&format!("{} more lines", 20 - TOOL_RESULT_MAX_LINES)));
    }

    #[test]
    fn random_hex_has_expected_length() {
        // n bytes → 2n hex chars.
        assert_eq!(random_hex(4).len(), 8);
        assert_eq!(random_hex(8).len(), 16);
    }
}
