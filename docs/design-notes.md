# Design notes

Why crew-watch is built the way it is. Nothing here is needed to *use* the tool
— see the [README](../README.md) for that.

crew-watch is a companion to
[firstmate](https://github.com/kunchenguid/firstmate) (upstream): the `TASK` and
`STATE` columns are read from a firstmate home. Those file formats are an
external contract owned by firstmate, which is why every read of them is
optional and best-effort — see [Robustness rules](#robustness-rules).

## Pure logic, separated from I/O and from rendering

Every non-trivial decision lives in a pure function with fixture-driven tests;
`src/ui.rs` is a thin mapper from those results to ratatui widgets, and the only
I/O is a single `/proc` pass plus a handful of small best-effort file reads.

| Module | Responsibility |
|--------|----------------|
| `src/procfs.rs` | pure `/proc` parsers + `collect()`, which reads `/proc` exactly once per tick |
| `src/detect.rs` | the `AGENT_KINDS` detection table, candidate extraction, and subtree aggregation into sessions |
| `src/meta.rs` | firstmate `state/*.meta` record parsing |
| `src/titles.rs` | task titles from `data/backlog.md` and `data/<task>/brief.md` |
| `src/taskinfo.rs` | the layered `TASK` resolution |
| `src/activity.rs` | `STATE` classification from the status verb and the turn record |
| `src/model.rs` | `--model` extraction from argv |
| `src/project.rs` | git-repository project name from a working directory |
| `src/quota.rs` | quota report parsing and the background poller |
| `src/quota_row.rs` | the pure quota-row builder |
| `src/quota_dialog.rs` | provider-selection dialog as a state machine over key codes |
| `src/config.rs` | the `key=value` config file |
| `src/agent_cols.rs` | agent-table column geometry and per-value fitting |
| `src/format_util.rs` | human-readable formatting, bars, reset labels |
| `src/app.rs` | per-tick state |
| `src/ui.rs` | rendering |

Several of these modules carry a header comment stating a contract that is easy
to break from the outside; `src/agent_cols.rs` and `src/quota_row.rs` in
particular are worth reading before touching layout.

## Resource cost

`/proc` is read exactly once per refresh: one directory scan, a `stat`,
`cmdline` and `cwd` read per pid, and the system files (`/proc/stat`,
`/proc/meminfo`, `/proc/loadavg`, `/proc/uptime`). The firstmate home — a
handful of small files — is re-read on the same tick.

That re-read is deliberate and load-bearing. Reading task records once at launch
would make a long-running `crew-watch` drift: tasks started later would never
get a title, and a worker directory recycled into a new task would keep showing
the previous occupant's label. Re-reading every tick costs a few small file
reads and keeps the table honest.

The quota fetch is the one thing that is *not* on the tick path. It shells out
to another tool that takes the better part of a second even when cached, and can
stall on the network, so it runs on a background thread at its own cadence with
a 10s kill-timeout and hands results over a channel that the main loop drains
without blocking. Every failure leg reduces to "no fresh quota data", never to a
stalled or dead monitor.

## Robustness rules

- A `/proc` entry that vanishes mid-read is skipped. Processes disappearing
  under the scan is the normal case, not an error.
- Every firstmate file is optional. Missing, unreadable or malformed input
  degrades exactly one signal (a title, a state glyph) and never fails a
  refresh.
- The quota payload is parsed schema-tolerantly: unknown fields are ignored,
  optional ones default, and an unrecognised schema version is not fatal. An
  additive upstream change keeps working; a genuinely broken payload reports the
  version it saw.
- Firstmate's turn record is only trusted when its generation token matches the
  armed generation. A stale incarnation classifies as *unknown*, never as idle —
  showing an agent as settled when it is mid-turn is the worse failure.

## Layout contracts

**Nothing wraps, nothing overflows, magnitudes are never truncated.** Numeric
cells are right-aligned, and ratatui truncates an over-width right-aligned line
from the *left* — which would silently drop the leading digits and turn
`1.4GiB` into something that reads as a much smaller number. Every numeric cell
is therefore pre-fitted to its column width by `src/agent_cols.rs`, shortening
by unit and precision (`848.6MiB` → `849MiB` → `849M`) instead of by
truncation.

**The task line shortens, the project prefix survives.** Under width pressure
the title is what gives; only when the column cannot hold the project name at
all does the project itself ellipsize.

**Quota rows share a grid.** Multiple provider rows use common column positions
so the bars stack. Alignment is the first fidelity dropped on a narrow terminal;
below that each row degrades through the same width ladder a single row uses,
and a lone provider is never padded for a column no other row needs. This
applies to the TUI only — `--once` output stays bar-free and unaligned so it
remains easy to script against.

**Window order is canonical, not source order.** Quota windows always render
`session`, then `week`, then the rest alphabetically by displayed label.
Classification is by the displayed label, so a new provider whose windows are
labelled the same way falls into order with no per-provider special-casing.

## The STATE glyphs

Every glyph must be a single scalar with East Asian Width = Wide and default
emoji presentation — no variation selector (U+FE0F), no ZWJ sequences. The
reason is that three layers disagree about the width of a VS16 sequence:
ratatui measures spans with a VS16-aware unicode-width, truncates through a
VS16-blind one, and the terminal advances by the base character's `wcwidth`. A
glyph like 🛠️ is measured as 2, 1, and 1-or-2 by those three, which shears the
whole column grid. A test pins the invariant, so a glyph that violates it fails
CI rather than the layout.

`--once` uses words instead of emoji for the same family of reasons: on a plain
stream, grep-ability beats glyphs, and a word column keeps character count equal
to display width.

## Scope

Linux-only by design for v1: the process model is `/proc`, and a portable
abstraction over it would cost more than it buys while the tool has one
platform. CPU% on the very first frame is 0 — there is no previous sample to
diff against yet — and becomes real from the second refresh onward.
