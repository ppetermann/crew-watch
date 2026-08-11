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
- `cargo test` — unit tests for the pure logic (parsing, detection, aggregation, meta, taskinfo). TUI rendering is intentionally untested in v1.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must stay clean (CI gates on them).
- `crew-watch --once` — non-interactive one-shot dump; the way to verify detection/CPU/mem outside a TTY.

### Architecture map (pure logic is split from I/O and from rendering)

- `src/procfs.rs` — pure parsers (`parse_proc_pid_stat`, `parse_proc_stat`, `parse_meminfo`, `parse_loadavg`, `parse_uptime`, `parse_cmdline`) + `collect()` which reads `/proc` exactly once per tick.
- `src/detect.rs` — `AGENT_KINDS` detection table, `extract_candidates`, `build_sessions` (subtree aggregation; nearest-enclosing-agent attribution so nested agents are separate rows excluded from ancestors).
- `src/meta.rs` + `src/taskinfo.rs` — firstmate `state/*.meta` parsing and layered `TASK` resolution.
- `src/ui.rs` — ratatui rendering. `src/app.rs` — tick state. `src/main.rs` — CLI + event loop.

### Sharp edge: building on a host without gcc

The captain's workstation this was built on had no system `cc`/`gcc` (and no
passwordless sudo). A `cc` shim at `~/.local/bin/cc` drives the `lld` bundled
with rustup and supplies the glibc PIE crt, so `cargo build` works without root.
On any normal host with `build-essential` installed, plain `cargo build` works
and the shim is unnecessary. CI (ubuntu-latest) has gcc by default.

