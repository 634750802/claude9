use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::process::{Command, ExitStatus, Stdio};

fn run_raw(args: &[String]) -> Result<(String, String)> {
    let out = Command::new("run9")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("failed to spawn run9; is it on PATH?")?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        bail!(
            "run9 {:?} failed (exit {}):\n{}",
            args,
            out.status.code().unwrap_or(-1),
            stderr.trim_end()
        );
    }
    Ok((stdout, stderr))
}

fn run_json(args: &[String]) -> Result<Value> {
    let (stdout, _) = run_raw(args)?;
    serde_json::from_str(&stdout)
        .with_context(|| format!("parsing run9 {args:?} output as JSON:\n{stdout}"))
}

pub fn box_inspect(box_id: &str) -> Result<Value> {
    run_json(&["box".into(), "inspect".into(), box_id.into()])
}

pub fn box_create_from_snap(
    name: Option<&str>,
    snap_id: &str,
    shape: &str,
    labels: &[(&str, &str)],
) -> Result<Value> {
    let mut args: Vec<String> = vec!["box".into(), "create".into()];
    if let Some(n) = name {
        args.push(n.into());
    }
    args.push("--snap".into());
    args.push(snap_id.into());
    args.push("--shape".into());
    args.push(shape.into());
    args.push("--description".into());
    args.push("Managed by claude9. Do not operate on this box directly.".into());
    for (k, v) in labels {
        args.push("--label".into());
        args.push(format!("{k}={v}"));
    }
    run_json(&args)
}

pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
}

fn build_exec_args(
    box_id: &str,
    user: &str,
    workdir: &str,
    env: &HashMap<String, String>,
    command: &[&str],
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "box".into(),
        "exec".into(),
        box_id.into(),
        "--user".into(),
        user.into(),
        "--workdir".into(),
        workdir.into(),
    ];
    for (k, v) in env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    args.push("--".into());
    for c in command {
        args.push((*c).into());
    }
    args
}

/// Run a command inside a box via `run9 box exec` (buffered).
///
/// `env` entries are passed as `-e KEY=VALUE` and show up as shell env vars
/// in the remote command — use this for large/untrusted prompt payloads so we
/// never have to shell-escape them.
pub fn box_exec(
    box_id: &str,
    user: &str,
    workdir: &str,
    env: &HashMap<String, String>,
    command: &[&str],
) -> Result<ExecResult> {
    let args = build_exec_args(box_id, user, workdir, env, command);
    let (stdout, stderr) = run_raw(&args)?;
    Ok(ExecResult { stdout, stderr })
}

pub fn box_exec_bg(
    box_id: &str,
    user: &str,
    workdir: &str,
    deadline: &str,
    env: &HashMap<String, String>,
    command: &[&str],
) -> Result<Value> {
    let mut args: Vec<String> = vec![
        "box".into(),
        "exec-bg".into(),
        box_id.into(),
        format!("--deadline={deadline}"),
        "--user".into(),
        user.into(),
        "--workdir".into(),
        workdir.into(),
    ];
    for (k, v) in env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    args.push("--".into());
    for c in command {
        args.push((*c).into());
    }
    run_json(&args)
}

pub fn box_exec_bg_pull(exec_id: &str, from_start: bool) -> Result<String> {
    let mut args: Vec<String> = vec![
        "box".into(),
        "exec-bg".into(),
        "pull-output".into(),
        exec_id.into(),
    ];
    if from_start {
        args.push("--from-start".into());
    }
    let (stdout, _) = run_raw(&args)?;
    Ok(stdout)
}

pub fn box_exec_bg_kill(exec_id: &str) -> Result<()> {
    run_raw(&[
        "box".into(),
        "exec-bg".into(),
        "kill".into(),
        exec_id.into(),
    ])?;
    Ok(())
}

/// Run a command inside a box via `run9 box exec -it`, letting the child
/// process inherit our stdin/stdout/stderr. Used to hand a real terminal
/// over to an interactive `claude` session — claude9 doesn't touch the
/// stream at all, just forwards our TTY down to run9 and returns whatever
/// exit status the remote process surfaces.
pub fn box_exec_interactive(
    box_id: &str,
    user: &str,
    workdir: &str,
    command: &[&str],
) -> Result<ExitStatus> {
    let mut args: Vec<String> = vec![
        "box".into(),
        "exec".into(),
        box_id.into(),
        "-it".into(),
        "--user".into(),
        user.into(),
        "--workdir".into(),
        workdir.into(),
        "--".into(),
    ];
    for c in command {
        args.push((*c).into());
    }

    let status = Command::new("run9")
        .args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("failed to spawn run9; is it on PATH?")?;
    Ok(status)
}
