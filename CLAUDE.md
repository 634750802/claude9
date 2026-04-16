# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`claude9` is a small Rust CLI that wraps the `run9` CLI. It forks a
pre-prepared "base box" from run9, clones a project's configured repos
into it, and runs `claude -p` turns inside it with session persistence.
See `README.md` for user-facing install/usage and `SKILL.md` for the
full command reference.

## Build / test / lint

```
cargo build            # debug
cargo build --release  # release binary at target/release/claude9
cargo test --all       # all unit tests (no integration suite yet)
cargo test <name>      # run a single test by path or substring

cargo fmt --all -- --check             # what CI runs
cargo clippy --all-targets -- -D warnings  # what CI runs (warnings = error)
```

CI (`.github/workflows/ci.yml`) runs fmt-check, clippy with
`-D warnings`, and `cargo test --all` on every PR. Clippy is strict —
fix every warning it flags, don't `#[allow(...)]` past it unless the
warning is genuinely wrong for the context.

Releases are cut manually via the `Release` workflow
(`workflow_dispatch` with `patch` / `minor` / `major`); it bumps
`Cargo.toml`, tags `vX.Y.Z`, and publishes prebuilt binaries for
`darwin-arm64` / `linux-amd64` / `linux-arm64`. Don't hand-edit the
version or tag.

## Architecture

Everything this binary does ultimately shells out to the `run9` CLI —
it never talks to any remote API directly. All subprocess calls go
through `src/run9.rs`, which is the single chokepoint for `run9 box
inspect / create / exec / exec-bg / exec-bg pull-output / exec-bg kill`.
If you need a new remote operation, add a wrapper there rather than
invoking `run9` from a subcommand.

Module layout:

- `main.rs` — pure dispatch, one arm per subcommand.
- `cli.rs` — clap definitions (one `Args` struct per command).
- `config.rs` — `.claude9/config.toml` schema, and the hard-coded
  remote contract constants (`REMOTE_USER="guy"`, `WORKSPACE`,
  `REPOS_DIR`). These are intentionally **not** user-configurable —
  they're part of the base-snap contract documented in SKILL.md. Don't
  add CLI flags to override them.
- `resolver.rs` — resolves a base box name to its snap id via
  `run9 box inspect`, with `CLAUDE9_BASE_SNAP_ID` as an env escape
  hatch for when the snap is `inuse`.
- `state.rs` — per-box files under `.claude9/state/<box-id>/`:
  `meta.toml` (immutable box facts), `session.txt` (claude session id),
  `history.jsonl` (append-only invocation log), `bg.toml` (current
  background task's exec_id). Only immutable facts live locally —
  task status is always fetched from run9, never guessed.
- `commands.rs` — one `pub fn` per subcommand. Contains the core
  `run_claude_task` → `poll_bg_task` loop that drives `claude -p` over
  `run9 box exec-bg`. ~750 lines; most PR-level change lands here.

### `.claude9/` discovery rule

`config::claude9_dir()` walks up from CWD looking for an ancestor that
already contains a `.claude9/` directory (same rule git uses for
`.git/`), with `$HOME` as a hard ceiling so a stray `~/.claude9` can
never be picked up silently. Two consequences:

1. Unrelated project groups stay isolated just by living in different
   directory trees — rely on this rather than adding explicit
   `--project` flags.
2. **Manual testing must use an isolated CWD.** Running a dev build
   from inside this repo, or from any ancestor that has a `.claude9/`,
   will use that directory's state. For safe live testing, `cd` into a
   scratch directory (e.g. `/tmp/c9-test`) before invoking the binary.

### Background task model (`claude9 task` / `resume`)

`run_claude_task` launches `claude -p` via `run9 box exec-bg
--deadline=10h` so the task survives local disconnects, then
`poll_bg_task` tails its output. Invariants to preserve when editing
this path:

- **One bg task per box.** `ensure_no_active_bg_task` probes
  `bg.toml`'s `exec_id` with `pull-output` — if the remote is still
  alive, refuse; if the probe fails, clear the stale record and
  proceed.
- **Remote is the only source of truth for status.** `bg.toml` stores
  `exec_id` / `started_at` / `prompt_snippet` only. Do not add a
  `status` field — it will drift.
- **Session id is saved the moment it appears in the stream**, before
  the task completes. This is load-bearing for recoverability: if the
  task is interrupted, `claude9 resume` must still work.
- **Ctrl+C detaches, it does not kill.** The handler just flips an
  `AtomicBool`; the loop exits while the remote exec keeps running.
  `claude9 join <box-id>` reattaches with `pull-output --from-start`.
- **`pull-output` errors retry up to `PULL_ERROR_LIMIT` (10).**
  Transient errors are expected right after `exec-bg` returns
  (backend not yet ready) and once the exec is reaped after
  completion. Don't tighten this without understanding both cases.

### Prompt payloads

Prompts go to the remote `claude` via a `-e C9_PROMPT=<...>` env var
(see `run9::build_exec_args` and the `"$C9_PROMPT"` reference in
`run_claude_task`). This keeps arbitrary user text — newlines, quotes,
`$`, backticks — from ever needing shell-escaping. When adding new
remote commands that take user-controlled strings, use the same
pattern instead of interpolating into the shell string.

## Keeping docs in sync

`SKILL.md` is a user-facing skill file that agents load to learn how
to drive `claude9`. It's shipped with the binary and referenced from
`README.md`. Any change to:

- the list of subcommands (`cli.rs`),
- the files written under `.claude9/state/<box-id>/` (`state.rs`),
- the base-box contract (`config.rs` constants, remote shell scripts),

must also update `SKILL.md`. CI does not check this — reviewers do.

`templates/CLAUDE.md.template` is a **remote** `CLAUDE.md` dropped
inside each spawned box for the `claude` running there. It is not
read by this repo's development tooling and shouldn't be confused
with this file.
