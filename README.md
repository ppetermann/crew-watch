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
│ S   RUNTIME   MODEL   PID   ELAPSED   CPU%   MEM   TASK     │
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
cores=20 mem=12.8GiB/30.6GiB swap=2.7GiB/128.0GiB tasks=1092 load=1.63 1.70 1.87 up=3d 0:19:57
STATE   RUNTIME    MODEL              PID    ELAPSED      CPU%          MEM  TASK
busy    opencode   glm-5.3        2097118      31:59     77.5%     848.6MiB  eve-members: public share link for a tenant's upcoming timers
wait    opencode   glm-5.3        2098092      31:32     75.5%       1.1GiB  crew-watch: right-align the ELAPSED, CPU% and MEM columns
human   claude     -               483889   51:34:23      1.0%     594.2MiB  interactive @ firstmate
...
quota claude   session 12% 1h14m  week 53% 3d2h  fable 50% 3d2h
quota zai      session 51% 2h40m  week 10% 16h4m  MCP month 0% 10d16h
```

## Layout

**Top — system overview (htop-style):**

- Per-core usage bars for **all** cores, in columns (20 on the target machine;
  renders whatever `/proc/stat` reports).
- Memory and swap bars with used/total figures.
- Task count, load average (1/5/15), and uptime.

**Bottom — agent list, one row per running agent session:**

- `STATE` — what the agent is doing right now, as one emoji in the TUI (whose
  header is a bare `S`, so the column costs two cells) and a plain word in
  `--once`; see the table below.
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

Numeric columns (`PID`, `ELAPSED`, `CPU%`, `MEM`) are right-aligned so
magnitudes stack and rows compare at a glance; text columns (`STATE`,
`RUNTIME`, `MODEL`, `TASK`) are left-aligned. The TUI table and `--once`
follow the same rule.

### STATE column

Each fleet row's state comes from firstmate's per-task state files (read
best-effort every refresh, keyed by the record's filename stem): the last
lifecycle verb of `state/<stem>.status` beats everything; within `working`,
the gen-validated `state/<stem>.busy-state` record splits busy from waiting.

| state                    | TUI  | `--once` | meaning                                            |
|--------------------------|------|----------|----------------------------------------------------|
| busy (mid-turn)          | 🔨   | `busy`   | lifecycle working, turn open right now             |
| waiting (between turns)  | 💤   | `wait`   | lifecycle working, settled between turns           |
| working, turn unknown    | 🚧   | `work`   | working but no valid busy signal (codex/kimi/grok/muse, stale gen) |
| needs decision           | ❓   | `ask`    | asked a question a human must answer               |
| blocked                  | 🛑   | `blocked`| reported itself stuck                              |
| paused                   | ⏳   | `paused` | deliberately idling on a known external wait       |
| done (process lingering) | ✅   | `done`   | reported done, process still alive                 |
| failed                   | ❌   | `failed` | reported failed                                    |
| interactive              | 👤   | `human`  | non-fleet session the captain is driving           |
| unknown (default)        | 🤖   | `-`      | everything else (non-fleet autonomous row)         |

On a narrow terminal the emoji column compresses to one cell and falls back
to single-character ASCII forms (`*`, `z`, `w`, `?`, `!`, `~`, `+`, `x`,
`@`, `.`).

The authoritative glyph, ASCII and word tables live in `src/activity.rs`
(`glyph()`, `ascii()`, `once_label()`).

On a terminal too narrow for the full table the TUI shrinks the fixed columns
and the values shorten by unit and precision first (`848.6MiB` → `849MiB` →
`849M`, `38:23:40` → `38:23` → `38h`), never by losing their leading digits —
so a magnitude is never misread. Nothing wraps or overflows at any width.

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
   via its cwd (the worktree path). The task line is prefixed with the
   **basename of the record's `project=` path** so the project is explicit,
   e.g. `crew-watch: right-align the ELAPSED, CPU% and MEM columns`. Under
   width pressure the title is what shortens; the project prefix survives
   (and only ellipsizes itself when the column cannot even hold it). With a
   project known, the old bracketed task-id suffix is dropped — it only ever
   carried the project, inferable from ids that happened to start with it; a
   record without `project=` degrades to the older
   `title [task-id]` form so the id still serves as reference. If no title is
   found at all, the task id is shown (prefixed by the project when known).
   This is read-only and best-effort: a missing/unparseable home, record,
   backlog, or brief never fails.

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
- **Window order is canonical, never source order.** Windows always render as
  `session` first, then `week`, then the provider's remaining windows sorted
  alphabetically by their displayed label — so every provider's line reads the
  same way regardless of the order the API happened to return, with no
  per-provider special-casing. This holds in the TUI and in `--once`.
- **Bars line up down the rows.** When several providers are shown, their rows
  share column positions, so the bars stack under each other even when labels or
  reset countdowns differ in width. Alignment is the first fidelity dropped on a
  narrow terminal — below that, each row degrades through the width ladder on
  its own, exactly as a single row does — and a lone provider is never padded
  for a column no other row needs. This is TUI-only: `--once` stays bar-free and
  unaligned, so its output stays easy to script against.
- **Cadence.** The fetch runs on a **background thread** at its own cadence
  (default 600s; `--quota-interval`, clamped 60..=3600) and is bounded by a 10s
  kill-timeout. It is **never** on the `/proc` tick path, which stays read-once
  per refresh. A 60s floor is enforced because the quota tool rate-limits under
  faster polling.
- **Provider selection governs visibility.** Press `p` to open a checkbox dialog
  whose list comes from whatever `quota-axi` actually reports (live, failing, and
  signed-out providers) — never a hardcoded set. Selection is per provider, not
  per window. A selected provider always gets its line, showing a dim status
  phrase (`sign-in required`, `unavailable`) when it has no usage windows.
  Freshness never hides anything: a provider serving cached data is dimmed and
  suffixed `stale`, not dropped. With no saved selection ("auto" mode) the row
  shows whichever providers currently report usage windows, re-evaluated every
  refresh; the dialog seeds its checkboxes from that same set. Saving replaces
  auto with your explicit list — including an empty one, which hides the row.
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
resolution, activity classification, quota parsing/row-building/dialog, and
config — is unit-tested with fixture data. The TUI rendering itself is not
unit-tested in v1.

## License

MIT
