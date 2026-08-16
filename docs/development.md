# Development

## Build, test, lint

```sh
cargo build                                  # debug build
cargo build --release                        # binary at target/release/crew-watch
cargo test                                   # unit tests
cargo fmt --all                              # format
cargo clippy --all-targets -- -D warnings    # lint
```

CI ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs
`cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test --all` on every pull request and on pushes to `main`. Both the
format check and the lint are hard gates, so run them before pushing.

The fastest way to check detection and aggregation against real processes is the
non-interactive dump:

```sh
cargo run --release -- --once
```

## What is tested

All the pure logic is unit-tested with fixture data: `/proc` parsing, runtime
detection, subtree aggregation, firstmate record and title parsing, task
resolution, model extraction, project-name resolution, activity classification,
quota parsing, quota-row building, the provider dialog state machine, and the
config file. The TUI rendering itself is not unit-tested — it is deliberately a
thin mapper over tested pure functions (see
[design-notes.md](design-notes.md)), so tests target the functions rather than
the frame.

When you add behaviour, add it to a pure function with a test, and keep the
renderer dumb.

## Adding an agent runtime

Add one row to `AGENT_KINDS` in [`src/detect.rs`](../src/detect.rs):

```rust
AgentKind {
    id: "newthing",
    display: "newthing",
    matches: &[Match::Exact("newthing"), Match::Prefix("newthing-")],
},
```

`Match::Exact` for short names that could collide with unrelated binaries,
`Match::Prefix` for runtimes that exec versioned or wrapper binaries
(`muse-bin-<version>`, `pi-launcher`). Nothing else needs to change: the help
line's "detecting:" list, the detection itself and the aggregation all read this
table.

If the runtime is normally launched through an interpreter (`node /opt/x/cli`),
check that the interpreter is in `INTERPRETERS` in the same file, so the real
program name is recovered from `argv[1]`.

## Building on a host without a system C compiler

Linking a Rust binary for a glibc target needs a C link chain. If the machine
has no `cc`/`gcc` and you cannot install one, a small `cc` shim on `PATH` that
drives the `lld` bundled with rustup and supplies the glibc PIE crt is enough to
make `cargo build` work without root. On any host with `build-essential` (or the
equivalent) installed, plain `cargo build` works and no shim is needed; CI on
`ubuntu-latest` has a compiler by default.

## Screenshots

The README images under `docs/img/` are rendered from a synthetic scene — fake
`state/*.meta`, `.status` and `.busy-state` records under a scratch `--fm-home`
plus stand-in processes — rather than from a real machine, so no private
repository or task name ends up in the docs. Regenerating them safely means
three things:

- **Synthetic input only.** Point `--fm-home` at a scratch directory you filled
  yourself. Never capture against a real firstmate home.
- **A private PID namespace.** Run the capture under `bwrap … --unshare-pid` (or
  an equivalent), so `crew-watch` can only see the stand-in processes and no
  real command line reaches the image.
- **A dedicated tmux socket.** Render on a socket created for the capture
  (`tmux -L <something-unique>`) and kill only that server afterwards; never
  drive a shared or default tmux server for a capture.

The panes used for the current images are 150x24 (TUI) and 150x16 (`--once`).
