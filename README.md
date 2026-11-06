# crew-watch

A terminal monitor for a machine that is running AI coding agents, built as a
companion to **[firstmate](https://github.com/kunchenguid/firstmate)**. The top
half is an htop-style system overview — per-core bars, memory, swap, load,
uptime. The bottom half is the part htop cannot give you: **one row per running
agent session**, with CPU and memory aggregated over that agent's entire process
subtree (an agent's real cost is mostly the compilers, test runners and tools it
spawns), plus the model it is running, how long it has been up, and a one-line
description of what it is working on. If your provider quotas are readable
locally, a compact usage row sits at the bottom.

![crew-watch running against a busy fleet](docs/img/tui.png)

**crew-watch is built for [firstmate](https://github.com/kunchenguid/firstmate)
fleets.** Point it at a firstmate home and every row picks up the task that
agent is working on and a live activity glyph — busy, waiting, blocked,
needs-decision, done — read straight from firstmate's own task state. That is
the `TASK` and `STATE` half of the table, and it is what the tool is for. It
reads upstream firstmate's file formats directly; no fork, plugin or
configuration is required.

Without a firstmate home it still runs and still earns its place — process
detection, subtree CPU/memory, model and elapsed all work on any box running
`claude`, `opencode`, `codex` or friends — but the `TASK` column falls back to
the project directory and `STATE` stays on the interactive/unknown glyphs.

Linux and macOS.

## Install

`crew-watch` is published on crates.io, so the easiest install is:

```sh
cargo install crew-watch
```

Every install path compiles locally, so you need a recent stable
[Rust toolchain](https://rustup.rs) and a working C compiler and link chain
(`build-essential` on Debian/Ubuntu, `gcc` on Fedora; both distros ship it in
the usual developer meta-packages). The C compiler builds the TLS code of the
OpenRouter client (the [`ring`](https://crates.io/crates/ring) crate); a
library build without default features needs only the link chain.

You can also install straight from the repository:

```sh
cargo install --git https://github.com/ppetermann/crew-watch
```

Or from a clone:

```sh
git clone https://github.com/ppetermann/crew-watch
cd crew-watch
cargo install --path .
```

Whichever route you take, the `crew-watch` binary lands in `~/.cargo/bin`, which
rustup already puts on your `PATH`. If you would rather not install it,
`cargo build --release` leaves the binary at `target/release/crew-watch`.

### Optional: the quota row

The provider-quota row at the bottom of the TUI is the one feature with an
external dependency. crew-watch does not talk to any provider itself — it shells
out to [`quota-axi`](https://www.npmjs.com/package/quota-axi), which reads your
local provider credentials and reports the usage windows:

```sh
npm i -g quota-axi
```

Without it, crew-watch works normally and **the quota row is simply absent** —
the layout is identical to a build without the feature, and no error is shown.
(The one exception: if you have explicitly picked providers with the `p` dialog,
a failing fetch shows a single dim `quota: unavailable (quota-axi not found)`
line instead of hiding, so an explicit choice never fails silently.) Use
`--no-quota` to disable the row and the background fetch outright.

The one provider crew-watch reads itself is
[OpenRouter](https://openrouter.ai), over HTTPS, when a key is available (see
[OpenRouter](#openrouter) below); it needs no quota-axi.

## Usage

```sh
crew-watch                       # interactive TUI, refresh every 2s
crew-watch --interval 1          # refresh every 1s
crew-watch --once                # one-shot text dump, no TTY needed
crew-watch --fm-home ~/agents/firstmate
crew-watch --no-quota            # hide the quota row, skip the quota fetch
crew-watch --quota-interval 120  # refresh quota every 120s (clamped 60..=3600)
crew-watch --openrouter-window 1w  # OpenRouter spend per model over the last week
```

| Flag | Meaning |
|------|---------|
| `--interval <secs>` | Refresh interval. Default `2`. |
| `--once` | Non-interactive one-shot dump to stdout (see below). |
| `--fm-home <dir>` | firstmate home to read task state from. Also settable via `CREW_WATCH_FM_HOME`; defaults to `~/agents/firstmate`. |
| `--no-quota` | Disable the quota row and its background fetch entirely. |
| `--quota-interval <secs>` | Seconds between quota refreshes. Default `600`, clamped to `60..=3600`. |
| `--openrouter-window <1h\|1d\|1w>` | With an OpenRouter management key: the window of the spend per model, ending now. Default `1d`. Also `CREW_WATCH_OPENROUTER_WINDOW`. |
| `--openrouter-key-name <name>` | With an OpenRouter management key: the name of the key the line is about. Default: the account's only enabled key, else all keys. Also `CREW_WATCH_OPENROUTER_KEY_NAME`. |
| `--openrouter-harness <id\|none>` | Without a management key: the harness whose own OpenRouter key the line reads (`claude`, `opencode`, `codex`, `grok`, `kimi`, `muse`, `pi`), or `none` to turn that off. Default: the harness of the session running in the firstmate home. Also `CREW_WATCH_OPENROUTER_HARNESS`. |
| `--help` / `--version` | The usual. |

An OpenRouter management key is read only from the `CREW_WATCH_OPENROUTER_KEY`
environment variable, never from a flag, since arguments are visible in the
process list. A value that is not shaped like a key is refused at start.

Key bindings inside the TUI:

| Key | Action |
|-----|--------|
| `q` / `Esc` / `Ctrl-C` | quit |
| `p` | choose which quota providers get a row (when the quota row is enabled) |
| `a` | about screen (version, license, project link); `a` / `q` / `Esc` closes |

Keybindings are deliberately minimal.

### `--once`

Collects two samples about a second apart (so the CPU percentages are real
deltas, not zeroes), prints the system summary and the agent table to stdout,
and exits. Use it to check detection outside a terminal, or to script a fleet
check:

![crew-watch --once output](docs/img/once.png)

The `--once` table is plain text with no emoji and no bars, so it stays easy to
grep and to diff.

## What the columns mean

**Top — system overview:** per-core usage bars for every core `/proc/stat`
reports, memory and swap bars with used/total figures, task count, load average
(1/5/15) and uptime.

**Bottom — one row per agent session:**

| Column | Meaning |
|--------|---------|
| `S` / `STATE` | What the agent is doing right now — an emoji in the TUI (the header is a bare `S` so the column costs two cells), a word in `--once`. See the table below. |
| `RUNTIME` | Which agent runtime it is: `claude`, `opencode`, `codex`, … |
| `MODEL` | The model it was started with, read from its `--model` argument with the provider prefix stripped for compactness (`zai-coding-plan/glm-5.3` → `glm-5.3`). `-` when there is no model flag, e.g. an interactive session. |
| `PID` | The session's root agent process. |
| `ELAPSED` | How long that process has been running: `H:MM:SS`, with shorter runs dropping leading units (`2:05`, `5s`), switching to days form (`41d16h`) from 1000 hours on. |
| `CPU%` | Aggregated over the agent's whole process subtree, normalized so one core = 100%. A multi-core subtree can exceed 100%, exactly as in htop. |
| `MEM` | Resident memory, likewise aggregated over the subtree. |
| `TASK` | A one-line description of what the agent is working on (see [Task descriptions](#task-descriptions)). |

Rows are sorted by aggregated CPU% descending. Numeric columns are right-aligned
so magnitudes stack; text columns are left-aligned. When no agent runtime is
running, the panel shows a hint instead of going blank.

On a narrow terminal nothing wraps or overflows: the fixed columns shrink and
values shorten by unit and precision first (`848.6MiB` → `849MiB` → `849M`,
`38:23:40` → `38:23` → `38h`), never by losing their leading digits, so a
magnitude is never misread.

### The STATE column

`STATE` is only meaningful for agents matched to a firstmate task; everything
else falls back to the interactive or unknown glyph.

| State | TUI | `--once` | Meaning |
|-------|-----|----------|---------|
| busy (mid-turn) | 🔨 | `busy` | working, and in a turn right now |
| waiting (between turns) | 💤 | `wait` | working, settled between turns |
| working, turn unknown | 🚧 | `work` | working, but the runtime publishes no turn signal |
| needs decision | ❓ | `ask` | asked a question a human must answer |
| blocked | 🛑 | `blocked` | reported itself stuck |
| paused | ⏳ | `paused` | deliberately idling on a known external wait |
| done (process lingering) | ✅ | `done` | reported done, process still alive |
| failed | ❌ | `failed` | reported failed |
| interactive | 👤 | `human` | a session a human is driving, not a task |
| unknown (default) | 🤖 | `-` | everything else, including autonomous agents with no task record |

On a terminal too narrow for a two-cell column the emoji fall back to single
ASCII characters (`*`, `z`, `w`, `?`, `!`, `~`, `+`, `x`, `@`, `.`). The
authoritative glyph, ASCII and word tables live in
[`src/activity.rs`](src/activity.rs).

## Supported runtimes

`crew-watch` scans `/proc` once per refresh and matches each process's command
name (the basename of `argv[0]`, with a fallback through interpreters such as
`node`) against one table:

| Runtime | Matches (basename) |
|---------|--------------------|
| claude | `claude`, `claude-*` |
| opencode | `opencode`, `opencode-*` |
| codex | `codex`, `codex-*` |
| grok | `grok`, `grok-*` |
| kimi | `kimi`, `kimi-*` |
| muse | `muse`, `muse-*` (covers `muse-bin-<version>`) |
| pi | `pi`, `pi-*` (covers `pi-launcher`) |

Prefixes rather than exact names, because some runtimes exec versioned or
wrapper binaries. Short names like `pi` match exactly so they do not collide
with lookalikes (`pip`, `ping`).

Test stand-ins are skipped. A program path counts as a stand-in when it runs
through a directory named exactly `fakebin` (the stub-tool directory test
suites put on `PATH`, e.g. `/tmp/suite.X/fakebin/claude`), or lies under a
firstmate test fixture root (a directory holding a `.fm-test-fixture` marker).
A process that has exited but not been reaped (a zombie) is never shown
either. A stand-in's processes count toward whichever real agent launched the
suite. A stand-in started by bare name through `PATH`, outside a fixture root,
carries no such evidence and still shows while it runs.

Adding a runtime is one row in `AGENT_KINDS` in
[`src/detect.rs`](src/detect.rs) — see [docs/development.md](docs/development.md).

### Sessions and nesting

A **session** is the top-most agent process in a subtree, and gets one row.
Every other process is attributed to its *nearest enclosing agent*, so an
agent's children — compilers, test runners, shells — are folded into its
CPU/memory aggregate rather than listed separately, and a nested agent (one
running inside another agent's subtree) gets its own row and is excluded from
its ancestor's totals.

## Task descriptions

The `TASK` column is resolved from layered sources; the first that answers wins.

1. **A firstmate task record**, when the process's working directory matches a
   task worktree. The line is the task's human title from the fleet backlog,
   prefixed with the project name — `crew-watch: right-align the numeric
   columns`. Under width pressure the title shortens and the project prefix
   survives.
2. **Otherwise, the project it is working in.** The project name is the git
   repository name when the working directory is inside one (worktrees and
   bare-repo worktrees included), else the directory's basename. A session with
   no prompt/headless argument reads as human-driven and shows
   `interactive @ <project>`; an autonomous one shows just `<project>`.
3. **No working directory:** a short excerpt of the meaningful positional
   arguments, else the runtime name. Bare flag noise (`-p --verbose`) is never
   shown.

## Configuration

### firstmate integration

This is what crew-watch is built for: the `TASK` and `STATE` columns come from
[firstmate](https://github.com/kunchenguid/firstmate)'s own task state.
Everything it reads is read-only and best-effort — a missing, unreadable or
unparseable file degrades that one signal and never fails a refresh.
`crew-watch` reads no other tool's files and writes nothing back.

`crew-watch` looks for a firstmate home in this order:

1. `--fm-home <dir>`
2. `$CREW_WATCH_FM_HOME`
3. `~/agents/firstmate`

Inside that directory it reads, per task:

| Path | Used for |
|------|----------|
| `state/<task>.meta` | matches an agent process to a task by working directory; supplies the project name |
| `state/<task>.status` | the task's lifecycle verb (`working`, `blocked`, `done`, …) → `STATE` |
| `state/<task>.busy-state` + `state/<task>.busy-gen` | splits `working` into busy vs. waiting-between-turns |
| `data/backlog.md` | the human task title for `TASK` |
| `data/<task>/brief.md` | fallback title when the backlog has none |

These are upstream firstmate's own file formats — no fork or plugin is needed,
and pointing `--fm-home` at any firstmate home works. Records are re-read on
every refresh, not once at launch, so a long-running `crew-watch` follows tasks
as they start, finish and get recycled.

### Config file

Quota provider selection persists to
`${XDG_CONFIG_HOME:-~/.config}/crew-watch/config` as simple `key=value` lines:

```
quota_providers=claude,codex
```

The file is written by the `p` dialog; unknown keys are preserved. There is
nothing else to configure — every other setting is a flag.

## Quota row

Above the help line, `crew-watch` shows one borderless line per selected
provider: session / week / per-model usage bars with a percentage and a reset
countdown.

```text
 claude session ████░░░░░░░░  34% 2h24m  week ███████░░░░░  61% 3d7h   fable ███░░░░░░░░░  22% 3d7h
 zai    session █████████░░░  78% 1h11m  week █████░░░░░░░  40% 4d14h
```

- **Source.** The row is populated by running `quota-axi --json` — the external
  [`quota-axi`](https://www.npmjs.com/package/quota-axi) tool
  (`npm i -g quota-axi`), not bundled with and not required by crew-watch. If it
  is not on your `PATH` nothing breaks — the row simply does not appear. Every
  quota failure is non-fatal and leaves the rest of the monitor working.
- **Window order is fixed:** `session`, then `week`, then the provider's
  remaining windows alphabetically, so every provider's line reads the same way.
- **Rows line up.** With several providers shown, the bars stack under each
  other even when labels and countdowns differ in width.
- **Cadence.** The fetch runs on a background thread at its own cadence
  (default 600s, `--quota-interval`) with a 10s timeout, never on the refresh
  path. The 60s floor is enforced because quota tooling rate-limits under faster
  polling.
- **Choosing providers.** Press `p` for a checkbox dialog listing whatever the
  quota tool actually reports, including signed-out and failing providers.
  With no saved selection the row shows whichever providers currently report
  usage; saving replaces that with your explicit list — including an empty one,
  which hides the row.
- **Staleness is shown, not hidden.** A provider serving cached data is dimmed
  and marked `stale`; after repeated fetch misses the whole row dims and gains
  an `(Xm old)` suffix while keeping the last good numbers.
- `--no-quota` disables the row and the background fetch entirely.

In `--once` the same data prints bar-free, one line per provider:

```text
quota claude   session 34% 2h24m  week 61% 3d7h  fable 22% 3d7h
```

### OpenRouter

crew-watch reads OpenRouter itself (no quota-axi involved) and shows it as an
`openrouter` provider, which replaces quota-axi's provider of the same id. The
key decides what the line shows:

| Key | What the line shows |
|-----|---------------------|
| A management key in `CREW_WATCH_OPENROUTER_KEY` | The key's credit limit left (or the account's credits left when the key has no limit, or when the balance is lower), and the spend over the last hour, day or week (`--openrouter-window`): the four most expensive models and the rest combined |
| Otherwise, the OpenRouter key of the harness in use | That key's limit (or the budget that blocks it first) and its spend in the current UTC day, week and month |
| Neither | No OpenRouter line; nothing is sent to OpenRouter |

```text
 openrouter limit ███░░░░░░░░░  26% 24d0h  left $74.50
            1d $3.20  gpt-4.1 $2.10 · gemini-3-flash-preview $0.80 · claude-sonnet-4 $0.21 · gpt-4o $0.07 · rest $0.02
```

With a harness's key the second line reads `today $3.20 · week $11.40 · month
$25.50`. A terminal too narrow for the second line cuts it from the right, so
the total and the most expensive models stay longest. `--once` prints both
lines, and when the harness has no usable key, one line saying why:

```text
quota openrouter  limit 26% 24d0h  left $74.50
quota openrouter  1d $3.20  gpt-4.1 $2.10, gemini-3-flash-preview $0.80, claude-sonnet-4 $0.21, gpt-4o $0.07, rest $0.02
quota openrouter: pi has no OpenRouter key
```

A refused key (expired, revoked, mistyped) shows `sign-in required`; a regular
key in `CREW_WATCH_OPENROUTER_KEY` shows `needs a management key`. A management
key that expires within a week adds a `key expires in …` note.

**The harness in use** is the harness of the agent session whose working
directory is the firstmate home (`--fm-home`); of several, the one running
longest. `--openrouter-harness` names one instead. crew-watch reads its key
fresh at each fetch, from the harness's own files or environment, in the
harness's own order:

| Harness | Where the key is read |
|---------|----------------------|
| Claude Code | `ANTHROPIC_AUTH_TOKEN` (then `ANTHROPIC_API_KEY`), only when `ANTHROPIC_BASE_URL` is OpenRouter's. Each from the `env` block of the project's `.claude/settings.local.json`, `.claude/settings.json`, then `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR`), else the environment |
| OpenCode | `provider.openrouter.options.apiKey` in `$OPENCODE_CONFIG_CONTENT`, then in `opencode.json`/`opencode.jsonc` (the project's, `$OPENCODE_CONFIG`, then `~/.config/opencode/`); then the `openrouter` entry of `~/.local/share/opencode/auth.json` (or `$OPENCODE_AUTH_CONTENT`); then `OPENROUTER_API_KEY` |
| pi | The `openrouter` entry of `~/.pi/agent/auth.json` (an API key, or the key from "Sign in with OpenRouter"); then `providers.openrouter.apiKey` in `models.json` beside it; then `OPENROUTER_API_KEY`. `$PI_CODING_AGENT_DIR` moves the directory |
| Codex | In `~/.codex/config.toml` (`$CODEX_HOME`), the provider `model_provider` names, when its `base_url` is OpenRouter's: `experimental_bearer_token`, else the variable `env_key` names |
| Grok Build | In `~/.grok/config.toml` (`$GROK_HOME`), the `[model."<name>"]` that `[models] default` names, when its `base_url` is OpenRouter's: the variable `env_key` names |
| Kimi Code | In `~/.kimi-code/config.toml` (`$KIMI_CODE_HOME`), the provider of `default_model`, when its `base_url` is OpenRouter's: `api_key`, else the variable `api_key_env` names, else `OPENAI_API_KEY` in its `env` table |
| Muse Code | Cannot use OpenRouter |

The rules: a key is used only where its harness would send it to OpenRouter
(its base URL must be `https` on `openrouter.ai` or a subdomain; a provider
pointed at a proxy gives no key). A key that comes from a command (pi's
`!command`, Claude Code's `apiKeyHelper`, Codex's `auth`) is never run, so
there is no line. Environment references are followed only as a whole value
(`$NAME`, `${NAME}`, `{env:NAME}`). In the TUI, environment variables are
crew-watch's own, which can differ from the session's; keys kept in the
harness's files are found either way.

Requests go over HTTPS to `openrouter.ai` only, with redirects and proxies off,
on the quota thread at the quota cadence: one request per fetch with a
harness's key, four with a management key (one more per hundred keys the
account has). Keys are never printed, logged or
passed to another program.

## Use as a library

Since 0.4.0 the crate is also a library, `crew_watch`, with the data behind the
TUI: the process snapshot, agent detection with subtree CPU/memory, and the
firstmate task, activity and PR-poll reads. Turn off the default `tui` feature
so the terminal UI and its dependencies are not compiled:

```toml
[dependencies]
crew-watch = { version = "0.5", default-features = false }
```

The OpenRouter reader and the harness key lookup sit behind the `openrouter`
feature (on by default, and implied by `tui`): add `features = ["openrouter"]`
to get `crew_watch::openrouter::fetch_quota`, which runs `quota-axi` and the
OpenRouter reader side by side and returns one report, plus the account's key
names and the lookup's outcome for a settings page. Without it the library
compiles no HTTP or TLS code.

Point a `crew_watch::fleet::Sampler` at a firstmate home (any home: the library
has no default path) and call `sample()` on an interval for a serializable
snapshot of the system and every agent session. The
[crate documentation](https://docs.rs/crew-watch/latest/crew_watch/) has a
runnable example.

Each `sample()` reads the system once, with blocking filesystem I/O, so run it
off an async runtime's worker threads. CPU figures are measured between two
samples, so the first one reads 0%. Every row carries the raw stamps behind its `STATE` (the last status
line's verb and `[at=]` time, the turn record's `ts=`) so a caller can judge
staleness itself, plus the pull request firstmate's merge poll is armed for.

Since 0.5.0 a fleet row also carries what a reader needs to tell where a task
stands in its delivery: the task's kind, mode, branch, registered PR and launch
time from its record, its whole status log (verb, time, decision key and text
per line), its last green local gate, the planner's estimate from the backlog
and when a scout's report was written. `crew_watch::ledger::LedgerTail` reads
firstmate's fleet activity ledger incrementally, for tasks that have already
been cleaned up.

## Requirements

- **Linux or macOS.** On Linux, `crew-watch` reads `/proc` directly. On macOS,
  it uses the [`sysinfo`](https://crates.io/crates/sysinfo) crate; agents owned
  by other users appear without task/cwd info unless run as root. macOS support
  ships since 0.2.0 — earlier releases were Linux-only.
- A recent stable Rust toolchain to build it — on macOS this means Xcode
  Command Line Tools, which provide the linker.
- Nothing else. There is no daemon, no config to write before first run, and no
  network access on the monitoring path — the system is sampled exactly once
  per refresh. The only request crew-watch makes itself is the OpenRouter one,
  on the quota thread, when a key is available.

## Documentation

- [docs/development.md](docs/development.md) — building, testing, linting, CI,
  and how to add a runtime.
- [docs/design-notes.md](docs/design-notes.md) — architecture, the layout
  contracts, and why some things are the way they are.

## License

MIT — see [LICENSE](LICENSE).
