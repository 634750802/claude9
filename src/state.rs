use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use crate::config::claude9_dir;

#[derive(Serialize, Deserialize, Debug)]
pub struct BoxMeta {
    pub box_id: String,
    pub base_box: String,
    pub snap_id: String,
    pub shape: String,
    pub created_at: DateTime<Utc>,
    pub projects: Vec<String>,
}

pub fn state_root() -> Result<PathBuf> {
    Ok(claude9_dir()?.join("state"))
}

pub fn box_dir(box_id: &str) -> Result<PathBuf> {
    let dir = state_root()?.join(box_id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn save_meta(meta: &BoxMeta) -> Result<()> {
    let path = box_dir(&meta.box_id)?.join("meta.toml");
    std::fs::write(&path, toml::to_string_pretty(meta)?)?;
    Ok(())
}

pub fn save_session(box_id: &str, session_id: &str) -> Result<()> {
    let path = box_dir(box_id)?.join("session.txt");
    std::fs::write(&path, session_id)?;
    Ok(())
}

pub fn load_session(box_id: &str) -> Result<String> {
    let path = box_dir(box_id)?.join("session.txt");
    let s = std::fs::read_to_string(&path)
        .with_context(|| format!("no saved session for {}", box_id))?;
    Ok(s.trim().to_string())
}

pub fn load_meta(box_id: &str) -> Result<BoxMeta> {
    let path = box_dir(box_id)?.join("meta.toml");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let meta: BoxMeta = toml::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(meta)
}

/// One user-triggered invocation against a box. Appended to
/// `.claude9/state/<box-id>/history.jsonl` so `talk` can show a
/// "last activity" hint when multiple boxes match a prefix, and so the
/// user has a local record of what they've asked on each box.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HistoryEntry {
    pub ts: DateTime<Utc>,
    /// `task` | `resume` | `talk`.
    pub kind: String,
    /// First ~200 chars of the prompt — enough to recognize the topic
    /// without the file ballooning on long seed documents.
    pub prompt_snippet: String,
    /// Claude's session id when known (one-shot task / resume). Absent
    /// for `talk` since we don't intercept the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

const HISTORY_SNIPPET_MAX: usize = 200;

pub fn append_history(box_id: &str, kind: &str, prompt: &str, session_id: Option<&str>) -> Result<()> {
    let path = box_dir(box_id)?.join("history.jsonl");
    let snippet: String = prompt.chars().take(HISTORY_SNIPPET_MAX).collect();
    let entry = HistoryEntry {
        ts: Utc::now(),
        kind: kind.to_string(),
        prompt_snippet: snippet,
        session_id: session_id.map(|s| s.to_string()),
    };
    let line = serde_json::to_string(&entry)?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{}", line)?;
    Ok(())
}

pub fn load_history(box_id: &str) -> Result<Vec<HistoryEntry>> {
    let path = box_dir(box_id)?.join("history.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Every box id under `.claude9/state/` whose directory name starts with
/// `<prefix>-`. Does not validate the prefix format; caller is expected
/// to have passed a user-supplied prefix string already.
pub fn list_box_ids_by_prefix(prefix: &str) -> Result<Vec<String>> {
    let root = state_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let needle = format!("{}-", prefix);
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&root)
        .with_context(|| format!("reading {}", root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            if name.starts_with(&needle) {
                ids.push(name.to_string());
            }
        }
    }
    Ok(ids)
}
