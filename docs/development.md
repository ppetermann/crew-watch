# Development

## Build, test, lint

```sh
cargo build                                  # debug build
cargo build --release                        # binary at target/release/crew-watch
cargo test                                   # unit tests
cargo fmt --all                              # format
cargo clippy --all-targets -- -D warnings    # lint

# the library alone, without the `tui` feature (no ratatui, crossterm, clap)
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test --no-default-features
```

CI ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs
`cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test --all`, then the clippy and test steps again with
`--no-default-features`, on every pull request and on pushes to `main`. Both
the format check and the lint are hard gates, so run them before pushing.

## Library and binary

One crate, two targets. `src/lib.rs` is the `crew_watch` library: every module
that does not draw, with `src/fleet.rs` (`Sampler`) as its entry point. The
`crew-watch` binary (`src/main.rs`) requires the default `tui` feature and owns
the terminal modules (`app`, `ui`, `agent_cols`, `about`, `quota_dialog`,
`quota_row`, `cli`); it imports the library's modules at its crate root, so
`crate::<module>` paths read the same in both. New logic that a non-terminal
caller could use belongs in the library; anything that renders stays in the
binary. Changing a `pub` item in the library is a change to the crate's public
API, so it needs the same care as a CLI flag.

The fastest way to check detection and aggregation against real processes is the
non-interactive dump:

```sh
cargo run --release -- --once
```

## What is tested

All the pure logic is unit-tested with fixture data: `/proc` parsing, runtime
detection, subtree aggregation,
[firstmate](https://github.com/kunchenguid/firstmate) record and title parsing,
task resolution, model extraction, project-name resolution, activity
classification, quota parsing, quota-row building, the provider dialog state
machine, and the config file. The TUI rendering itself is not unit-tested — it
is deliberately a thin mapper over tested pure functions (see
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
  (`tmux -S <private path>` or `tmux -L <something-unique>`) and kill only that
  server afterwards; never drive a shared or default tmux server for a capture.

`scripts/screenshots/capture.sh` does all of this and writes both PNGs:

```sh
scripts/screenshots/capture.sh   # ~3 minutes; OUT_DIR=<dir> to write elsewhere
```

It runs `cargo build --release` first, so the images always show the code as
it stands (set `CREW_WATCH_BIN=<path>` to capture a specific binary instead).
It needs `bwrap`, `tmux`, `git`, `python3` and `google-chrome`, plus network
access for the web fonts. The steps, each in its own file under
`scripts/screenshots/`:

1. **Scene** (`scene.sh`, run inside `bwrap --unshare-pid`): fills a scratch
   firstmate home (`state/*.meta`, `.status`, `.busy-state`/`.busy-gen`,
   `data/backlog.md`), puts a fake `quota-axi` on `PATH`, and starts stand-in
   agents (`standin.py`, run through `python3` under each runtime's name, with
   a set CPU load and resident size) in three waves a minute apart so ELAPSED
   differs between rows. Edit the `scene` table there to change the rows.
2. **Capture**: runs `crew-watch` (150x24) and `crew-watch --once` (150x16) in
   a tmux server on a socket inside the scratch directory, waits until the
   quota row has drawn, and saves each pane with `tmux capture-pane -e -p`
   (text plus colour escapes).
3. **Render**: `ansi2html.py` turns each capture into a fixed-grid HTML page
   (JetBrains Mono, Noto Color Emoji for the STATE glyphs, a One Dark palette),
   and headless Chrome screenshots it at 2x:
   `google-chrome --headless=new --force-device-scale-factor=2
   --window-size=<w>,<h> --screenshot=<out>.png <page>.html`.

The system panel (CPU and memory meters, task count, load average, uptime)
shows the capturing host: the PID namespace hides other processes from the
agent table, not the host-wide counters under `/proc`.
