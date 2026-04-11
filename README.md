# claude9

Spawn `run9` dev boxes preloaded for Claude-based work. `claude9` forks
a base box, clones a project's configured repos into it, and runs
`claude -p` tasks inside the box with session persistence for resume.

```bash
claude9 config                       # create .claude9/config.toml
claude9 spawn --name my-box          # create a box, sync repos
claude9 task   my-box "do a thing"   # stream a claude -p turn
claude9 resume my-box "and another"  # continue the same session
```

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/634750802/claude9/main/install.sh | sh
```

Specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/634750802/claude9/main/install.sh | sh -s v0.1.0
```

Custom install directory:

```bash
curl -fsSL https://raw.githubusercontent.com/634750802/claude9/main/install.sh | CLAUDE9_INSTALL_DIR=~/.local/bin sh
```

Prebuilt binaries are published on [GitHub Releases](https://github.com/634750802/claude9/releases)
for `darwin-arm64`, `linux-amd64`, and `linux-arm64`.

## Configuration

`claude9 config` creates `./.claude9/config.toml` in the current project
group. The location is discovered by walking up from CWD to the nearest
ancestor containing a `.claude9/` directory (git-style), with `$HOME` as
a ceiling — so unrelated project groups stay isolated.

```toml
[defaults]
base_box = "claude-remote-base"   # name of the run9 box to fork from
shape    = "8c16g"                # run9 shape for spawned boxes

[[projects]]
repo = "owner/repo"
# Optional: name = "alias"  (local dir under /home/guy/workspace/repos/)
```

Per-box state (meta + last claude session id) is persisted under
`.claude9/state/<box-id>/`. Add `/.claude9/` to your project's `.gitignore`.

## Commands

| Command | Purpose |
|---|---|
| `claude9 config` | Create / open `.claude9/config.toml` in `$EDITOR` |
| `claude9 spawn [OPTIONS]` | Create a box, clone repos, optionally run an inline task |
| `claude9 task <box-id> [PROMPT...]` | Stream one `claude -p` turn on the box |
| `claude9 resume <box-id> [PROMPT...]` | Continue the last session on the box |

`spawn` options: `--name`, `--task` / `--task-file`, `--no-update`,
`--base-box`, `--shape`. `task` / `resume` accept either positional
prompt tokens or `-f <file>`.

## Base box contract

`claude9` doesn't provision anything automatically. The base box you
point it at must already have, at minimum:

- A remote user named `guy` with `/home/guy/workspace` writable
- A `claude` CLI authenticated as `guy`, supporting `-p`,
  `--output-format stream-json --verbose`, and `--resume <sid>`
- A `gh` CLI authenticated as `guy` (for private-repo cloning)
- A `git` global identity set for `guy`

Everything else (language toolchains, build tools, ...) is up to you.
See [`SKILL.md`](SKILL.md) for the full contract and the list of
non-goals.

## Agent Skill

`claude9` ships with a skill file so AI agents know how to drive it.
Drop it into your Claude Code skills directory, or inline it into your
project's `CLAUDE.md`:

| Skill file | Use case |
|------------|----------|
| [`SKILL.md`](SKILL.md) | Let Claude Code (or another agent) invoke `claude9 spawn` / `task` / `resume` on your behalf |

## Build from source

```bash
git clone https://github.com/634750802/claude9.git
cd claude9
cargo build --release
# binary at target/release/claude9
```
