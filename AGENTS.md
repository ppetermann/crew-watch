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
runtime, with subtree-aggregated CPU/mem). Linux-only v1. See `README.md` for
the full user-facing spec (install, usage, detection table, firstmate records).

### Build / test / lint (authoritative commands)

- `cargo build --release` — binary at `target/release/crew-watch`.
- `cargo test` — unit tests for the pure logic (parsing, detection, aggregation, meta, taskinfo, quota). TUI rendering is intentionally untested in v1.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must stay clean (CI gates on them).
- `crew-watch --once` — non-interactive one-shot dump; the way to verify detection/CPU/mem outside a TTY.

### Architecture map (pure logic is split from I/O and from rendering)

- `src/procfs.rs` — pure parsers (`parse_proc_pid_stat`, `parse_proc_stat`, `parse_meminfo`, `parse_loadavg`, `parse_uptime`, `parse_cmdline`) + `collect()` which reads `/proc` exactly once per tick.
- `src/detect.rs` — `AGENT_KINDS` detection table, `extract_candidates`, `build_sessions` (subtree aggregation; nearest-enclosing-agent attribution so nested agents are separate rows excluded from ancestors).
- `src/meta.rs` — firstmate `state/*.meta` parsing. `src/titles.rs` — backlog.md + brief.md title lookup. `src/taskinfo.rs` — layered `TASK` resolution (fleet title → cwd project → argv). `src/model.rs` — `--model` argv extraction. `src/project.rs` — git-repo project-name resolution from cwd (handles worktrees).
- `src/quota.rs` — `quota-axi --json` parse (serde, schema-tolerant) + ISO-8601→epoch + background poller (`fetch_once`/`spawn_poller`, 10s kill-timeout, **never on the `/proc` tick path**). `src/quota_row.rs` — pure row builder: per-provider `build_provider_line` + multi-provider `build_quota_rows` (aligned block). Its two layout contracts — **canonical window order** and **aligned columns**, including how each degrades on a narrow terminal — are specified in that file's module header; read it before touching quota-row layout. `src/quota_dialog.rs` — provider-selection dialog (pure state machine over `KeyCode`). `src/config.rs` — `key=value` config file (`~/.config/crew-watch/config`, `quota_providers=`), same read-tolerance idiom as `meta.rs`.
- `src/ui.rs` — ratatui rendering. `src/format_util.rs` — human-readable formatting + the shared `make_bar`/`make_bar_min_one`/`format_reset` used by both the system meters and the quota row. `src/app.rs` — tick state; owns `fm_home` and re-reads fleet records + titles every tick (see the `src/taskinfo.rs` header for why that freshness is load-bearing); quota state is drained from the poller off the tick path. `src/cli.rs` — clap CLI. `src/main.rs` — entry point + event loop.

### Sharp edge: building on a host without gcc

The captain's workstation this was built on had no system `cc`/`gcc` (and no
passwordless sudo). A `cc` shim at `~/.local/bin/cc` drives the `lld` bundled
with rustup and supplies the glibc PIE crt, so `cargo build` works without root.
On any normal host with `build-essential` installed, plain `cargo build` works
and the shim is unnecessary. CI (ubuntu-latest) has gcc by default.

