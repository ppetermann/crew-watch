# crew-watch

TUI fleet monitor: **htop-style system overview on top; a per-agent view of
running AI agent runtimes and their whole process-subtree cost below.**

`htop`'s bottom half is a flat process list that does not answer the question
the captain actually has: *which AI agents are running, what are they doing, and
what do they cost?* `crew-watch` replaces that bottom half with one row per
agent session, with CPU% and memory aggregated over the agent's entire subtree
(the agent's real cost is mostly its child compilers, tests, and tools).

![layout: top = per-core bars + mem/swap + tasks/load/uptime; bottom = agents table]

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
```

Inside the TUI:

| Key            | Action |
|----------------|--------|
| `q` / `Esc`    | quit   |
| `Ctrl-C`       | quit   |

Keybindings are intentionally minimal for v1.

### `--once`

Non-interactive mode: collects two samples ~1s apart (so CPU% deltas are real),
then prints the system summary and the agent table to stdout and exits. Use it
to verify detection or to script a fleet check without a terminal:

```
$ crew-watch --once
cores=20 mem=4.4GiB/30.6GiB swap=0KiB/128.0GiB tasks=630 load=4.03 3.77 3.11 up=2:11:33
RUNTIME        PID    ELAPSED      CPU%          MEM  TASK
opencode     28404      36:34     97.0%     927.2MiB  crew-watch-v1 (crew-watch)
claude       14405      41:27     18.0%     516.1MiB  fm-daemon-exit-on-return (firstmate)
...
```

## Layout

**Top — system overview (htop-style):**

- Per-core usage bars for **all** cores, in columns (20 on the target machine;
  renders whatever `/proc/stat` reports).
- Memory and swap bars with used/total figures.
- Task count, load average (1/5/15), and uptime.

**Bottom — agent list, one row per running agent session:**

- `RUNTIME` — which agent runtime it is (`claude`, `opencode`, ...).
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
wins. The layering mirrors the detection table so other fleet formats can be
added later.

1. **Firstmate fleet records**, when present. If the firstmate home (env
   `CREW_WATCH_FM_HOME`, or default `~/agents/firstmate`) contains
   `state/*.meta` files, each records `worktree=`, `project=`,
   `endpoint_task_id=`, etc. `crew-watch` matches an agent process to a record
   via its cwd (the worktree path) and shows the task id and project, e.g.
   `crew-watch-v1 (crew-watch)`. This is read-only and best-effort: a
   missing/unparseable home or record never fails.
2. **Fallback**: the process's cwd basename plus a trimmed argv excerpt.

If neither yields anything, the runtime name is shown.

## Design notes & constraints

- **Resource-light.** `/proc` is read exactly once per refresh: one directory
  scan plus per-pid `stat` / `cmdline` / `cwd` reads and the system files
  (`/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, `/proc/uptime`).
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
parsing, and task resolution — is unit-tested with fixture data. The TUI
rendering itself is not unit-tested in v1.

## License

MIT
