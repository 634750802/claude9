use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Hard-coded contract with the base snap. Not user-configurable.
pub const REMOTE_USER: &str = "guy";
pub const WORKSPACE: &str = "/home/guy/workspace";
pub const REPOS_DIR: &str = "/home/guy/workspace/repos";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub defaults: Defaults,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub claude: ClaudeOptions,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Defaults {
    pub base_box: String,
    pub shape: String,
}

/// User-configurable flags forwarded to every `claude -p` invocation.
/// All fields are optional; empty / none means "don't pass the flag, let
/// claude use its own default".
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ClaudeOptions {
    /// `--model <MODEL>`. Accepts either a short alias for the latest
    /// version in a family (`opus`, `sonnet`, `haiku`) or a fully
    /// qualified model id (`claude-opus-4-6`). None = let claude pick
    /// its own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// `--effort <LEVEL>`. Claude accepts one of:
    /// `low`, `medium`, `high`, `max`. Controls how much the model
    /// "thinks" before answering — `max` is the heaviest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,

    /// `--permission-mode <MODE>`. Claude accepts one of:
    /// `default`, `acceptEdits`, `bypassPermissions`, `plan`.
    /// Headless mode can't show approval prompts, so set this (or
    /// `dangerously_skip_permissions`) if you need tools like WebFetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,

    /// When `true`, pass `--dangerously-skip-permissions` to every
    /// `claude -p` invocation. Skips every permission check — use only
    /// on a box you fully trust / that's ephemeral.
    #[serde(default)]
    pub dangerously_skip_permissions: bool,

    /// `--allowedTools` — explicit allow-list. Passed as a comma-joined
    /// single arg, e.g. `["WebFetch", "Bash(git:*)"]` becomes
    /// `--allowedTools 'WebFetch,Bash(git:*)'`.
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// `--disallowedTools` — explicit deny-list, same serialization as
    /// `allowed_tools`.
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Project {
    pub repo: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl Project {
    pub fn local_name(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        self.repo
            .rsplit('/')
            .next()
            .unwrap_or(&self.repo)
            .to_string()
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            defaults: Defaults {
                base_box: "claude-remote-base".into(),
                shape: "8c16g".into(),
            },
            projects: vec![
                Project {
                    repo: "db9-ai/db9-backend".into(),
                    name: None,
                },
                Project {
                    repo: "db9-ai/db9-server".into(),
                    name: None,
                },
                Project {
                    repo: "db9-ai/db9-build".into(),
                    name: None,
                },
                Project {
                    repo: "tidbcloud/db9-cd".into(),
                    name: None,
                },
            ],
            claude: ClaudeOptions::default(),
        }
    }
}

/// Resolve the project-local `.claude9/` directory.
///
/// Walks up from the current working directory looking for the nearest
/// ancestor that already contains a `.claude9/` directory — same discovery
/// rule git uses for `.git/`. If no ancestor has one, falls back to
/// `<cwd>/.claude9`, so the next `ensure_exists()` creates it there.
///
/// `$HOME` is a ceiling: the walk stops before entering HOME, so a
/// stray `~/.claude9` (e.g. left over from an earlier claude9 version)
/// can never be picked up from a project subdir and silently take over.
/// This also keeps distinct project groups (different CWDs) isolated.
pub fn claude9_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    for ancestor in cwd.ancestors() {
        if let Some(h) = &home {
            if ancestor == h {
                break;
            }
        }
        let candidate = ancestor.join(".claude9");
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Ok(cwd.join(".claude9"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(claude9_dir()?.join("config.toml"))
}

pub fn ensure_exists() -> Result<PathBuf> {
    let path = config_path()?;
    if !path.exists() {
        std::fs::create_dir_all(claude9_dir()?)?;
        let text = toml::to_string_pretty(&Config::default())?;
        std::fs::write(&path, text)?;
    }
    Ok(path)
}

pub fn load() -> Result<Config> {
    let path = ensure_exists()?;
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_local_name_uses_explicit_alias() {
        let p = Project {
            repo: "owner/repo".into(),
            name: Some("alias".into()),
        };
        assert_eq!(p.local_name(), "alias");
    }

    #[test]
    fn project_local_name_defaults_to_repo_basename() {
        let p = Project {
            repo: "owner/repo".into(),
            name: None,
        };
        assert_eq!(p.local_name(), "repo");
    }

    #[test]
    fn project_local_name_handles_repo_without_slash() {
        let p = Project {
            repo: "standalone".into(),
            name: None,
        };
        assert_eq!(p.local_name(), "standalone");
    }

    #[test]
    fn config_default_round_trips_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.defaults.shape, cfg.defaults.shape);
        assert_eq!(parsed.projects.len(), cfg.projects.len());
    }

    #[test]
    fn claude_options_serde_skips_none_and_empty() {
        // skip_serializing_if should keep unset fields out of the
        // scaffolded config so users don't see a wall of `= null` lines.
        let opts = ClaudeOptions::default();
        let text = toml::to_string(&opts).unwrap();
        assert!(!text.contains("model"));
        assert!(!text.contains("effort"));
        assert!(!text.contains("permission_mode"));
    }
}
