use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
