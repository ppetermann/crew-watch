# crew-watch

TUI fleet monitor: **htop-style system overview on top; a per-agent view of
running AI agent runtimes and their whole process-subtree cost below.**

`htop`'s bottom half is a flat process list that does not answer the question
the captain actually has: *which AI agents are running, what are they doing, and
what do they cost?* `crew-watch` replaces that bottom half with one row per
agent session, with CPU% and memory aggregated over the agent's entire subtree
(the agent's real cost is mostly its child compilers, tests, and tools).

```text
┌─ system overview ───────────────────────────────────────────┐
│ per-core usage bars · mem/swap bars · tasks / load / uptime │
├─ agents ────────────────────────────────────────────────────┤
│ RUNTIME   MODEL   PID   ELAPSED   CPU%   MEM   TASK         │
├─────────────────────────────────────────────────────────────┤
│ quota row: one line per provider (session/week/model bars)  │
└─────────────────────────────────────────────────────────────┘
```

## Status

Linux-only v1. Built and verified against real `claude` and `opencode`
processes (subtree CPU/memory attribution, firstmate record matching).

## Install

```sh
cargo install --path .
# or, from a clone:
cargo build --release      # binary at target/release/crew-watch
```

Requirements: a recent stable Rust toolchain and (as for any native Rust binary
on a glibc target) a working C link chain. Standard distros have this out of the
box via `build-essential` / `gcc-c++`.

## Usage

```sh
crew-watch                 # interactive TUI, refresh every 2s
crew-watch --interval 1    # refresh every 1s
crew-watch --once          # one-shot text dump (no TTY) — handy for scripts/CI
crew-watch --fm-home ~/agents/firstmate
crew-watch --no-quota      # hide the quota row and skip the quota-axi fetch
crew-watch --quota-interval 120  # refresh quota every 120s (clamped 60..=3600)
```

Inside the TUI:

| Key            | Action |
|----------------|--------|
| `q` / `Esc`    | quit   |
| `Ctrl-C`       | quit   |
| `p`            | choose which quota providers get a row (when the quota row is enabled) |

Keybindings are intentionally minimal for v1.

### `--once`

Non-interactive mode: collects two samples ~1s apart (so CPU% deltas are real),
then prints the system summary and the agent table to stdout and exits. Use it
to verify detection or to script a fleet check without a terminal:

```
$ crew-watch --once
cores=20 mem=4.4GiB/30.6GiB swap=0KiB/128.0GiB tasks=630 load=4.03 3.77 3.11 up=2:11:33
RUNTIME    MODEL            PID    ELAPSED      CPU%          MEM  TASK
opencode   glm-5.2       28404      36:34     97.0%     927.2MiB  crew-watch: add MODEL column ... [crew-watch-model-task-cols]
claude     opus          14405      41:27     18.0%     516.1MiB  away mode unusable: resurface fires ... [fm-afk-resurface-loop]
claude     -              5463     2:50:31      0.0%     560.0MiB  interactive @ firstmate
...
quota claude   session 5% 1h45m  week 48% 3d22h  fable 45% 3d22h
```

## Layout

**Top — system overview (htop-style):**

- Per-core usage bars for **all** cores, in columns (20 on the target machine;
  renders whatever `/proc/stat` reports).
- Memory and swap bars with used/total figures.
- Task count, load average (1/5/15), and uptime.

**Bottom — agent list, one row per running agent session:**

- `RUNTIME` — which agent runtime it is (`claude`, `opencode`, ...).
- `MODEL` — the model the agent is running, parsed from its `--model` argv flag
  with the provider prefix stripped for compactness
  (`zai-coding-plan/glm-5.2` → `glm-5.2`). Shows `-` when no model flag is
  present (e.g. an interactive session).
- `PID` / `ELAPSED` — the session's root agent process.
- `CPU%` and `MEM` — aggregated over the agent's whole process subtree. CPU% is
  normalized so one core = 100% (a multi-core subtree can exceed 100%, matching
  `htop`).
- `TASK` — a one-line description of what the agent is working on (see below).

Rows are sorted by aggregated CPU% descending. If no agent runtime is running,
the panel shows an empty-state hint instead of going blank.

## Detection

`crew-watch` scans `/proc` once per tick and matches each process's command name
(basename of argv[0], with a fallback through known interpreters like `node`)
against a single detection table:

| runtime  | id        | matches (basename)                           |
|----------|-----------|-----------------------------------------------|
| claude   | `claude`  | `claude`, `claude-*`                          |
| opencode | `opencode`| `opencode`, `opencode-*`                      |
| codex    | `codex`   | `codex`, `codex-*`                            |
| grok     | `grok`    | `grok`, `grok-*`                              |
| kimi     | `kimi`    | `kimi`, `kimi-*`                              |
| muse     | `muse`    | `muse`, `muse-*` (covers `muse-bin-<version>`)|
| pi       | `pi`      | `pi`, `pi-*` (covers `pi-launcher`)           |

Patterns — not exact names — are used on purpose, because some runtimes exec
versioned or wrapper binaries (e.g. a `muse` launcher execs `muse-bin-<version>`,
`pi` may run as `pi-launcher`). Exact matches for short names (like `pi`) avoid
collisions with lookalikes (`pip`, `ping`).

**To add a runtime:** add one row to `AGENT_KINDS` in
[`src/detect.rs`](src/detect.rs). That is the only change needed.

### Sessions and nesting

An **agent session** is the top-most agent process in a subtree. Each detected
agent is one row. Every process is attributed to its *nearest enclosing agent*,
so:

- the agent's children (compilers, test runners, shells, ...) are folded into
  the agent's CPU/memory aggregate and not listed separately; and
- a **nested agent** (an agent running inside another agent's subtree) gets its
  own row and is **excluded from the ancestor's aggregate**.

## Task info sources ("what is it working on")

The `TASK` column is resolved by layered sources, in order; the first to answer
wins.

1. **Firstmate fleet records + backlog title**, when present. If the firstmate
   home (env `CREW_WATCH_FM_HOME`, or default `~/agents/firstmate`) contains
   `state/*.meta` files, each records `worktree=`, `project=`,
   `endpoint_task_id=`, etc. `crew-watch` matches an agent process to a record
   via its cwd (the worktree path). When the task id resolves to a human title
   from `data/backlog.md` (or, as a fallback, the first sentence of
   `data/<task-id>/brief.md`), the column shows that title with the task id in
   brackets, e.g. `away mode unusable: resurface fires ... [fm-afk-resurface-loop]`.
   If no title is found, the task id is shown on its own. This is read-only and
   best-effort: a missing/unparseable home, record, backlog, or brief never
   fails.

   Records and titles are re-read on **every refresh**, not once at launch, so a
   long-running `crew-watch` follows task lifecycle: a task started after launch
   shows its own title, and a worker copy recycled into a new task (firstmate
   reuses worktree paths) follows the new task rather than the previous
   occupant. When a record disappears, its agent falls through to the layers
   below instead of keeping the dead task's label.

2. **Unmatched agent, has a cwd**: the project name as a human-readable label.
   The project name is the git repo name when the cwd is inside a git repo
   (handles worktrees and bare-repo worktrees like no-mistakes), else the cwd
   basename. An interactive session shows `interactive @ <project>`; an
   autonomous session whose task could not be identified shows just
   `<project>`. Bare flag noise (`-p --verbose`) is never shown.
3. **No cwd**: a trimmed positional argv excerpt, else the runtime name.

The MODEL column is parsed independently from the agent's `--model` argv flag
(both `--model X` and `--model=X`), with any provider prefix
(`org/model-family` → `model-family`) stripped. No flag → `-`.

## Quota row

Directly above the help line, crew-watch shows **one borderless line per
selected quota provider** — session / week / per-model usage bars in crew-watch's
own `█░` style, plus a percent and a reset countdown, degrading through a
deterministic width ladder on narrow terminals:

```text
 claude session █░░░░░░░░░░░   5% 1h45m  week ██████░░░░░░  48% 3d22h  fable █████░░░░░░░  45% 3d22h
```

- **Source.** `quota-axi --json` (schema 3 today, parsed schema-tolerantly — an
  additive field or an unknown `schemaVersion` is ignored, not fatal). One
  provider = one line of its windows; a future provider (e.g. z.ai) appears with
  zero code change.
- **Cadence.** The fetch runs on a **background thread** at its own cadence
  (default 300s; `--quota-interval`, clamped 60..=3600) and is bounded by a 10s
  kill-timeout. It is **never** on the `/proc` tick path, which stays read-once
  per refresh. A 60s floor is enforced because the quota tool rate-limits under
  faster polling.
- **Provider selection governs visibility.** Press `p` to open a checkbox dialog
  whose list comes from whatever `quota-axi` actually reports (live, failing, and
  signed-out providers) — never a hardcoded set. Selection is per provider, not
  per window. A selected provider always gets its line, showing a dim status
  phrase (`sign-in required`, `unavailable`) when it has no usage windows.
  Freshness never hides anything: a provider serving cached data is dimmed and
  suffixed `stale`, not dropped. On the first run with no saved selection the
  selection is *seeded* from the providers currently reporting usage windows
  ("auto") — a seed, not an ongoing filter, so your later choices win.
  Explicitly clearing the list hides the row.
- **Persistence.** The selection is stored at
  `${XDG_CONFIG_HOME:-~/.config}/crew-watch/config` as
  `quota_providers=claude,codex`. The file format is `key=value` (no TOML dep);
  unknown keys are preserved on save.
- **Failure behaviour.** Every quota failure is non-fatal and leaves the core
  monitor working: a missing binary in auto mode shows no row; an explicit
  selection whose fetch fails shows one dim `quota: unavailable (...)` line;
  a provider that is signed-out or erroring shows a dim status phrase only when
  selected; after repeated fetch misses the row dims and gains an `(Xm old)`
  suffix while keeping the last good report.
- **`--no-quota`** disables the row and the background fetch entirely (the
  layout is then byte-identical to a build without the feature).

In `--once` the same data prints bar-free, one line per effective provider:
`quota claude   session 5% 1h45m  week 48% 3d22h  fable 45% 3d22h`.

## Design notes & constraints

- **Resource-light.** `/proc` is read exactly once per refresh: one directory
  scan plus per-pid `stat` / `cmdline` / `cwd` reads and the system files
  (`/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, `/proc/uptime`). The
  firstmate home — a handful of small files — is re-read on the same tick, for
  the freshness reason described under *Task info sources*.
- **Robust to churn.** A `/proc` entry that vanishes mid-read is skipped —
  `crew-watch` never panics on a disappearing process.
- **Linux-only** for v1.
- CPU% on the very first frame is 0 (no previous sample yet); it becomes real
  from the second refresh onward.

## Development

```sh
cargo build
cargo test
cargo fmt
cargo clippy --all-targets -- -D warnings
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, and `cargo test` on every pull
request.

The pure logic — `/proc` parsing, agent detection, subtree aggregation, meta
parsing, model extraction, title lookup, project-name resolution, task
resolution, quota parsing/row-building/dialog, and config — is unit-tested with
fixture data. The TUI rendering itself is not unit-tested in v1.

## License

MIT
