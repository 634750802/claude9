---
name: claude9
description: Spawn a run9 dev box preloaded with a project group's repos and run claude -p tasks inside it with session persistence. Use when the user wants to run claude work against a fresh remote box, sync a configured list of repos into it, or follow up on a previous claude session by box id.
---

# claude9

CLI that spawns `run9` boxes from a base snap, clones the configured project
repos into the box, and runs `claude -p` tasks inside it with session
persistence for resume.

## When to use

Reach for `claude9` when the user wants to:

- Spin up a **fresh remote dev box** with a known set of repos already cloned.
- Fire a **one-shot `claude -p` task** against that box and see it stream live.
- **Resume a previous claude session** on a specific box by id.
- Drop into an **interactive claude session** on a named box (spawn-or-reuse)
  with a seed prompt.
- Work on multiple **project groups** on one machine — each project tree has
  its own `.claude9/config.toml` and `.claude9/state/` and they stay isolated.

Do **not** use `claude9` for:

- Running claude locally (just call `claude` directly).
- General box management — it only does
  `spawn` / `task` / `resume` / `interactive` / `bash` / `config`.
  No `ls`, `stop`, `rm`, `attach`. Use `run9` directly for those.

## Concepts

- **Base box** — a pre-customized run9 box (default `claude-remote-base`)
  whose snap is forked on every `spawn`. See the **Base box contract**
  section below for exactly what must be preinstalled on it.
- **Project group** — a directory tree whose root contains `.claude9/config.toml`.
  The config lists which repos get cloned into spawned boxes and any overrides
  to box shape / base box name.
- **Box id** — every spawned box has a short id. When `--name` is given
  it's used as a prefix with a random suffix appended (e.g.
  `--name db9` → `db9-a1b2c3d4`); otherwise run9 auto-allocates one
  (e.g. `plum-ant`). All subsequent `task` / `resume` calls are keyed by
  this id. `spawn` prints it at the end and also saves metadata under
  `.claude9/state/<box-id>/`.
- **Session id** — claude's own session id, saved in
  `.claude9/state/<box-id>/session.txt` so `resume` can continue the same
  conversation.

## Base box contract

Every `claude9 spawn` forks the base box's snap. `claude9` does not
provision anything automatically — the base box is prepared **once, by
hand**, and whatever users, tools, and auth state are on it at fork time
are what every spawned box inherits.

Beyond that, what "prepared" means is your call: install and configure
whatever the cloned repos and your claude workflows expect. The only
things `claude9` itself shells out to on the remote side are:

- **`claude`** — must support `-p` / `--print`, `--output-format stream-json`
  together with `--verbose`, and `--resume <session-id>`. Already
  authenticated as the remote user.
- **`gh`** — used by `spawn` to clone configured repos. Already
  authenticated as the remote user (so private repos work via gh's
  token).
- **`git`** — used by `spawn` for `fetch` / `pull --ff-only` on repos
  that already exist in the repos dir. Global `user.name` / `user.email`
  set on the remote user.

Everything else (language toolchains, build tools, shell helpers, ...)
is up to you — `claude9` never touches those.

See the **Layout** section below for the remote user and workspace
paths `claude9` assumes.

To set the base box up, drop into a shell on it:

```sh
claude9 bash
```

This targets `defaults.base_box` from config.toml and lands you in
`/home/guy/workspace` as `guy`. If you need root (for package installs
etc.), escalate from there (e.g. `sudo -i`) — claude9 intentionally
doesn't expose a `--user` flag, since everything else in the remote
contract is keyed on `guy`.

Refreshing the base box later works the same way — `claude9 bash` back
in, install the new thing, and the next `claude9 spawn` picks it up.
There's no explicit "rebuild snap" step in the normal case. If
`run9 box create` ever complains that the base box's snap is `inuse`,
see the **Gotchas** section.

## Commands

### `claude9 config`

Create `./.claude9/config.toml` with defaults if it doesn't exist, print its
path, and open it in `$EDITOR` (if set). Run this first in any new project
group to edit the repo list.

Config shape:

```toml
[defaults]
base_box = "claude-remote-base"   # name of the base box to fork from
shape    = "8c16g"                # run9 shape for spawned boxes

[[projects]]
repo = "owner/repo"
# Optional:
# name = "alias"   # local dir name; defaults to basename of repo

[claude]
# All fields optional. Omitted = let claude use its own default.
model = "opus"                        # alias or full id (claude-opus-4-6)
effort = "max"                        # low | medium | high | max
# permission_mode = "bypassPermissions" # default | acceptEdits | bypassPermissions | plan
dangerously_skip_permissions = true   # skip every permission check (ephemeral boxes only)
# allowed_tools = ["WebFetch", "Bash(git:*)"]
# disallowed_tools = []
```

### `claude9 spawn [OPTIONS]`

Create a new box from the base snap, wait for `ready` (180 s timeout), clone
every configured repo into `/home/guy/workspace/repos/<name>` inside the box,
and persist metadata. Optionally run a claude task immediately.

```
claude9 spawn [--name <prefix>]
              [--desc <purpose>]
              [--task <prompt> | --task-file <path>]
              [--no-update]
              [--base-box <name>]
              [--shape <shape>]
```

- `--name` — name prefix for the box; a random 8-hex suffix is appended
  (e.g. `--name db9` → `db9-a1b2c3d4`). Omit to let run9 auto-allocate.
- `--desc` — short description of what the box is for; stored as a
  `claude9-task` label on the box (visible via `run9 box ls --label claude9-task`).
- `--task` / `--task-file` — run an inline claude task after the box is ready;
  its session id gets saved to `.claude9/state/<box-id>/session.txt`.
- `--no-update` — skip git pull/clone entirely. Use when the base snap already
  has fresh checkouts and you want to boot fast.
- `--base-box` / `--shape` — per-invocation overrides of config defaults.

Every spawned box carries a fixed description
(`Managed by claude9. Do not operate on this box directly.`) and labels:
`claude9=managed`, `claude9-base=<base>`, `claude9-owner=<$USER>`,
and optionally `claude9-task=<desc>`.

Env escape hatch: set `CLAUDE9_BASE_SNAP_ID=<snap-id>` to bypass
`run9 box inspect` and pin an explicit snap id. Useful when the base box is
currently running (so its live snap is `inuse`) and you want to target a
pre-forked detached snap instead.

### `claude9 task <box-id> [PROMPT...]`

Run `claude -p --output-format stream-json --verbose "<prompt>"` on the box.
Streams assistant text and tool-use markers live as they arrive. Saves the
final `session_id` to `.claude9/state/<box-id>/session.txt`, overwriting any
previous one. Exits non-zero if claude reports an error.

```sh
claude9 task db9-a1b2c3d4 "audit the db9-server package for N+1 queries"
claude9 task db9-a1b2c3d4 -f ./prompt.md
```

### `claude9 resume <box-id> [PROMPT...]`

Read the saved session id, then run
`claude -p --resume <sid> --output-format stream-json --verbose "<prompt>"`.
Same streaming display. Fails loudly if no session is saved for the box id.

```sh
claude9 resume db9-a1b2c3d4 "now draft a fix for the worst three"
```

Claude's `--resume` reuses the same session id by default, so
`session.txt` effectively stays put across resumes (unless `--fork-session`
is ever added).

### `claude9 interactive [OPTIONS]`

Find-or-spawn a box, then hand a TTY over to an interactive `claude`
session running inside it. claude9 does not intercept stdout / stderr —
run9 gets the terminal directly, so the experience matches running
`claude` locally.

```
claude9 interactive [--name <prefix>]
                    [--first-prompt <text> | --first-prompt-file <path>]
                    [--model <MODEL>] [--effort <LEVEL>]
                    [--desc <purpose>]
```

- `--name` — optional prefix used to look up `.claude9/state/<prefix>-*`.
  0 matches → spawn a fresh box with that prefix (same flow as
  `claude9 spawn --name`). 1 match → reuse it. >1 matches → prompt on
  stdin to pick by index, listing each box with `created_at` + last
  activity + last-prompt snippet. Omit entirely to always spawn a fresh
  auto-named box (e.g. `plum-ant`), no reuse — good for one-off sessions.
- `--first-prompt` / `--first-prompt-file` — seed the session with an
  initial user message. Passed to `claude` as its positional arg, so no
  shell-escaping needed on your side. Omit for a blank interactive start.
- `--model` / `--effort` — per-invocation override of `[claude].model` and
  `[claude].effort`, same shape as the spawn-time `--base-box` / `--shape`.
- `--desc` — only used when spawning a new box; mirrors
  `claude9 spawn --desc` and sets the `claude9-task` label.

Interactive sessions do **not** get persisted to `session.txt` (claude9
doesn't read the stream). If you want to later `claude9 resume` against
the same conversation, use `task` / `resume` instead — those are the
session-managed surface.

```sh
claude9 interactive --name db9 --first-prompt-file primer.md
```

### `claude9 bash [BOX] [-- BASH_ARGS...]`

Transparent passthrough to `run9 box exec -it <box> --user guy --workdir
/home/guy/workspace -- /bin/bash`. Used for hand-preparing the base box
or for jumping onto any run9 box without memorizing the `run9` incantation.

- `BOX` — optional positional; defaults to `defaults.base_box` from
  config.toml. Pass any run9 box name / id to target something else
  (e.g. a spawned box you want to inspect manually).
- `--` — anything after is forwarded to `bash` as its own args, so
  `claude9 bash -- -lc 'echo hi'` runs a one-shot command and exits,
  while bare `claude9 bash` drops into an interactive shell.

`user` and `workdir` are **fixed** to `guy` / `/home/guy/workspace` —
the same remote contract `task` / `resume` / `interactive` already use.
If you need root, escalate from inside the shell.

```sh
claude9 bash                               # interactive shell on base box
claude9 bash db9-a1b2c3d4                  # interactive shell on a spawned box
claude9 bash -- -lc 'ls /home/guy/workspace/repos'
```

## Task history (`.claude9/state/<box-id>/history.jsonl`)

Every `task` / `resume` / `interactive` invocation appends one JSONL line
to `history.jsonl` alongside the box's other state:

```json
{"ts":"...","kind":"task","prompt_snippet":"...","session_id":"..."}
```

`interactive` entries have no `session_id` (we don't see the stream) and
their `prompt_snippet` is the seed prompt, possibly empty. The
interactive picker uses the newest entry to show each box's last
activity when multiple boxes match a prefix.

## Typical workflows

### First-time setup in a new project group

```sh
cd /path/to/project-group
claude9 config                          # create .claude9/config.toml, edit repo list
claude9 spawn --name db9 --desc "fix auth token refresh #327"
# → box id printed, e.g. db9-a1b2c3d4; repos cloned inside
claude9 task db9-a1b2c3d4 "first prompt"
claude9 resume db9-a1b2c3d4 "follow up on the same session"
```

### Spawn + inline task

```sh
claude9 spawn --task "summarize repos/db9-backend/README.md"
# box id is printed at the end; note it for follow-ups.
```

### Fast boot, no repo sync

```sh
claude9 spawn --name quick --no-update
```

### Targeting a pre-forked golden snap

```sh
CLAUDE9_BASE_SNAP_ID=svabcd1234 claude9 spawn --name db9
```

### Interactive topic-box with a primer

```sh
cat > /tmp/primer.md <<'EOF'
We're investigating slow cold-start on db9-server. See context doc at ...
Open questions: 1) ... 2) ...
EOF

# First run spawns `db9topic-xxxxxxxx` because nothing matches the prefix.
claude9 interactive --name db9topic --first-prompt-file /tmp/primer.md

# Later, back in the same project dir, the same command reuses the box.
claude9 interactive --name db9topic --first-prompt "follow up question"
```

## Layout

Created by `claude9` in the project tree:

```
<project>/.claude9/
├── config.toml
└── state/
    └── <box-id>/
        ├── meta.toml      # box_id, base_box, snap_id, shape, created_at, projects[]
        ├── session.txt    # last claude session id (from task / resume)
        └── history.jsonl  # append-only log of task / resume / interactive invocations
```

Hard-coded inside the remote box (contract with the base snap):

```
remote user:      guy
workspace:        /home/guy/workspace
repos dir:        /home/guy/workspace/repos
repo local path:  /home/guy/workspace/repos/<name>
```

`workspace/` may contain other subdirs (`memory/`, `knowledges/`, `notes/`, ...);
`claude9` only touches `repos/`.

## Discovery rules

`.claude9/` is located by walking up from the current working directory to
the nearest ancestor containing a `.claude9/` directory — same rule git uses
for `.git/`. `$HOME` is a ceiling: the walk stops before entering it, so a
stray `~/.claude9` from an older version can never silently hijack a project.

Practical consequence: you can invoke `claude9` from any subdirectory of a
project group and still find the right config and state. Two unrelated
project groups stay isolated just by living in different directory trees.

## Gotchas

- **`.claude9/` should be gitignored** — it holds per-box session state that
  isn't shared between collaborators. Add `/.claude9/` to the project's
  `.gitignore`.
- **Base box must be reachable** — `spawn` calls `run9 box inspect <base_box>`
  to read `box_snap_id`. If that inspect fails, set `CLAUDE9_BASE_SNAP_ID`.
- **A running base box holds its snap exclusively.** If `run9 box create`
  errors with `box is still running`, the live snap is `inuse` and can't be
  cloned. Either stop the base box first, or point `CLAUDE9_BASE_SNAP_ID` at
  a pre-forked detached snap.
- **`spawn` is serial** — repos are cloned one at a time. A single failing
  repo is reported at the end but doesn't abort the rest.
- **`task` / `resume` share one session file per box** — kicking off a new
  `task` on a box overwrites the previous `session.txt`, so `resume` will
  follow the most recent conversation only.
- **No cleanup commands.** To delete a box, use `run9 box rm <box-id>`
  directly and then remove `.claude9/state/<box-id>/` by hand.

## Non-goals (v1)

Not yet implemented, don't suggest these as if they work:

- `claude9 ls` / `stop` / `rm` / `doctor`
- Parallel repo sync
- Remote-control / streaming of interactive sessions (`interactive` hands
  off to `run9 box exec -it` and doesn't intercept the stream)
- Persisting interactive session ids so `claude9 resume` can follow them
  (resume only follows prior `task` / `resume` turns)
- Managing `memory/` / `knowledges/` / `notes/` inside the box's workspace
- Automatically provisioning the base box — see the **Base box contract**
  section for what you set up manually once, before using `claude9` at all
