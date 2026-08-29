# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Add durable project-specific notes here as they are discovered through real work.

## Maintaining this file

Keep this file for knowledge useful to almost every future agent session in this project.
Do not repeat what the codebase already shows; point to the authoritative file or command instead.
Prefer rewriting or pruning existing entries over appending new ones.
When updating this file, preserve this bar for all agents and keep entries concise.

## crew-watch

Rust TUI: htop-style system overview + a per-agent table (one row per running AI
runtime, with subtree-aggregated CPU/mem). Linux and macOS.

**Built for [firstmate](https://github.com/kunchenguid/firstmate)** (upstream —
link that URL, never a fork, wherever firstmate is referenced). The `TASK` and
`STATE` columns are read from a firstmate home's own `state/` and `data/` files;
crew-watch consumes upstream firstmate's formats read-only and writes nothing
back, so it must keep working against any firstmate home without a fork or
plugin. Treat those file formats as an external contract owned by firstmate.

Docs are split by audience and that split is load-bearing — keep it. `README.md`
is user-facing only (install, usage, columns, STATE table, detection table,
quota row, configuration); `docs/development.md` is contributor-facing;
`docs/design-notes.md` holds the architecture and the rationale behind the
layout/robustness contracts. Nothing fleet-internal or operator-specific belongs
in `README.md`.

### Build / test / lint (authoritative commands)

- `cargo build --release` — binary at `target/release/crew-watch`.
- `cargo test` — unit tests for the pure logic (parsing, detection, aggregation, meta, taskinfo, activity, quota). TUI rendering is intentionally untested in v1.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must stay clean (CI gates on them).
- `crew-watch --once` — non-interactive one-shot dump; the way to verify detection/CPU/mem outside a TTY.

### Delivery

The fleet-wide flow: branch → local ladder → DRAFT PR → ready-flip → one CI run + one
review round → merge. Drafts run no CI.

- **Local ladder = the authoritative commands above**, in order: fmt --check, clippy
  -D warnings, test. Green locally means green in CI (same commands).
- **PRs open as drafts, always** (`gh pr create --draft`); flip on this PR's landing
  turn after rebasing onto main and re-running the ladder: `gh pr ready <n>`
  (undo: `gh pr ready <n> --undo`).
- **Review gate:** the `ocr-review` check + one approving review (branch protection:
  `check`, `check-macos`, `ocr-review` required, stale reviews dismissed, last push
  must be approved).
- **Release tail (batched):** one release PR bumps `version` in Cargo.toml and updates
  README floors/notes (seed the notes with
  `git log v<prev>..HEAD --merges --format='- %s (%h)'`). After it merges:
  `git tag -a vX.Y.Z <merge-sha> -m "crew-watch X.Y.Z" && git push origin vX.Y.Z` —
  **the tag push IS the publish** (the Publish workflow releases to crates.io via
  trusted publishing; no token). Then rebuild the operator's PATH binary (target dir
  outside the clone). Cut a release when a user-facing fix waited >48h, ≥3 merged PRs
  are unreleased, or the operator asks.

### Architecture map (pure logic is split from I/O and from rendering)

- `src/procfs.rs` — pure parsers (`parse_proc_pid_stat`, `parse_proc_stat`, `parse_meminfo`, `parse_loadavg`, `parse_uptime`, `parse_cmdline`) + Linux `collect()` which reads `/proc` exactly once per tick; the `Snapshot` struct is the platform contract. `src/macos/` — macOS backend: `backend.rs` (`sysinfo`-based `collect`, mutex-guarded state on the tick thread) + `convert.rs` (pure, test-built on every platform: fabricates `/proc`-shaped cumulative `CpuLine` counters from per-core usage% — read its tests before touching the math).
- `src/detect.rs` — `AGENT_KINDS` detection table, `extract_candidates`, `build_sessions` (subtree aggregation; nearest-enclosing-agent attribution so nested agents are separate rows excluded from ancestors).
- `src/meta.rs` — firstmate `state/*.meta` parsing. `src/titles.rs` — backlog.md + brief.md title lookup. `src/taskinfo.rs` — layered `TASK` resolution (fleet title → cwd project → argv). `src/model.rs` — `--model` argv extraction. `src/project.rs` — git-repo project-name resolution from cwd (handles worktrees). `src/activity.rs` — STATE-column classification: parses firstmate's `state/<stem>.status` verb + gen-guarded `.busy-state`/`.busy-gen` turn record into one `Activity` (lifecycle verb beats turn state); glyphs are pinned to single Wide scalars (no VS16/ZWJ — widths disagree across unicode-width 0.1/0.2/wcwidth and shear the grid; test-enforced), `--once` uses words, not emoji.
- `src/quota.rs` — `quota-axi --json` parse (serde, schema-tolerant: reads both schema 3 windows, which carry `percentUsed`, and schema 5 (quota-axi ≥0.1.30) windows, which carry `percentRemaining` mapped to `100 − r`; whichever field is present wins, no version gate) + ISO-8601→epoch + background poller (`fetch_once`/`spawn_poller`, 10s kill-timeout, **never on the `/proc` tick path**). `src/quota_row.rs` — pure row builder: per-provider `build_provider_line` + multi-provider `build_quota_rows` (aligned block). Its two layout contracts — **canonical window order** and **aligned columns**, including how each degrades on a narrow terminal — are specified in that file's module header; read it before touching quota-row layout. `src/quota_dialog.rs` — provider-selection dialog (pure state machine over `KeyCode`). `src/config.rs` — `key=value` config file (`~/.config/crew-watch/config`, `quota_providers=`), same read-tolerance idiom as `meta.rs`.
- `src/about.rs` — about overlay: dismiss-key semantics, width-wrapped identity content (version from `CARGO_PKG_VERSION`), centered geometry; a `show_about` view flag on `App`, never a mode.
- `src/ui.rs` — ratatui rendering. `src/agent_cols.rs` — pure agent-table column geometry (`compressed_col_widths`, `task_width`) + per-value fitting for the right-aligned numeric columns; its module header explains why every such cell must be pre-fitted (ratatui truncates an over-width right-aligned line from the **left**, dropping the magnitude digits) — read it before touching agent-table widths or alignment. `src/format_util.rs` — human-readable formatting + the shared `make_bar`/`make_bar_min_one`/`format_reset` used by both the system meters and the quota row. `src/app.rs` — tick state; owns `fm_home` and re-reads fleet records + titles every tick (see the `src/taskinfo.rs` header for why that freshness is load-bearing); quota state is drained from the poller off the tick path. `src/cli.rs` — clap CLI. `src/main.rs` — entry point + event loop.

### Sharp edge: building on a host without gcc

Linking needs a C link chain. On a host with no system `cc`/`gcc` and no
passwordless sudo, a `cc` shim on `PATH` that drives the `lld` bundled with
rustup and supplies the glibc PIE crt makes `cargo build` work without root. On
any normal host with `build-essential`, plain `cargo build` works and the shim
is unnecessary; CI (ubuntu-latest) has gcc by default. See
`docs/development.md`.

### Sharp edge: regenerating the README screenshots

`docs/img/*.png` must never be captured against the real fleet: a synthetic
scratch `--fm-home` plus stand-in processes only, inside a private PID
namespace, on a dedicated `tmux -L <unique>` socket that is killed afterwards.
Never drive the default tmux server for a capture. Procedure:
`docs/development.md`.

