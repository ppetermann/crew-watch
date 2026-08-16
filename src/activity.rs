//! Per-agent activity classification for the STATE glyph column.
//!
//! Sources (both firstmate-owned, read best-effort like [`crate::meta`]),
//! keyed by the meta record's filename stem (`state/<stem>.status`, …):
//! - `state/<stem>.status` — append-only lifecycle log; the last line's verb
//!   (`working`, `needs-decision`, `blocked`, `paused`, `done`, `failed`, ...)
//!   is the task's lifecycle state.
//! - `state/<stem>.busy-state` + `state/<stem>.busy-gen` — firstmate's
//!   semantic turn-state contract (owner: firstmate `bin/fm-busy-lib.sh`),
//!   one line: `v1 gen=<tok> seq=<uint> state=<busy|idle|unknown>
//!   source=<tok> event=<tok> ts=<epoch>`. A record whose `gen` does not
//!   match the armed gen sidecar is a stale incarnation and classifies
//!   unknown — never idle, never busy.
//!
//! Lifecycle beats turn state: a `done` task with a lingering process shows
//! done, not idle. Within `working`, the turn state splits actively-in-a-turn
//! from waiting-between-turns; harnesses without a busy writer (muse, grok,
//! codex, kimi) have no record and render the working-no-signal glyph.
//!
//! ### Glyphs
//!
//! Every glyph MUST be a single scalar with East Asian Width = Wide and
//! default emoji presentation (no VS16 / U+FE0F, no ZWJ). ratatui measures
//! spans with unicode-width 0.2 (VS16-aware) but truncates through
//! unicode-truncate → unicode-width 0.1 (VS16-blind), and tmux/terminals
//! advance by wcwidth of the base char — a VS16 sequence like 🛠️ (U+1F6E0
//! U+FE0F) is measured 2, 1, and 1-or-2 by those three layers respectively
//! and shears the column grid. The `glyphs_are_single_wide_scalars` test
//! pins the invariant.

use std::fs;
use std::path::Path;

/// What an agent row is doing right now, as rendered in the STATE column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Activity {
    /// Fleet task, lifecycle working, turn open (mid-turn).
    Busy,
    /// Fleet task, lifecycle working, between turns (settled/idle).
    Waiting,
    /// Fleet task, lifecycle working, no valid turn signal (unarmed harness,
    /// stale gen, malformed record).
    Working,
    /// Fleet task asked a question a human must answer.
    NeedsDecision,
    /// Fleet task reported itself stuck.
    Blocked,
    /// Fleet task deliberately idling on a known external wait.
    Paused,
    /// Fleet task reported done (process still lingering).
    Done,
    /// Fleet task reported failed.
    Failed,
    /// Non-fleet session a human is driving interactively.
    Interactive,
    /// Anything else: non-fleet autonomous session, or no signal at all.
    #[default]
    Unknown,
}

impl Activity {
    /// Two-cell emoji for the STATE column. Single Wide scalars only — see
    /// the module header.
    pub fn glyph(self) -> &'static str {
        match self {
            Activity::Busy => "\u{1F528}",         // 🔨
            Activity::Waiting => "\u{1F4A4}",      // 💤
            Activity::Working => "\u{1F6A7}",      // 🚧
            Activity::NeedsDecision => "\u{2753}", // ❓
            Activity::Blocked => "\u{1F6D1}",      // 🛑
            Activity::Paused => "\u{23F3}",        // ⏳
            Activity::Done => "\u{2705}",          // ✅
            Activity::Failed => "\u{274C}",        // ❌
            Activity::Interactive => "\u{1F464}",  // 👤
            Activity::Unknown => "\u{1F916}",      // 🤖
        }
    }

    /// Short word for the `--once` STATE column: `--once` writes to a plain
    /// stream where grep-ability beats glyphs, and a word column keeps char
    /// count equal to display width (emoji are one char but two cells, which
    /// would shear the aligned prefix).
    pub fn once_label(self) -> &'static str {
        match self {
            Activity::Busy => "busy",
            Activity::Waiting => "wait",
            Activity::Working => "work",
            Activity::NeedsDecision => "ask",
            Activity::Blocked => "blocked",
            Activity::Paused => "paused",
            Activity::Done => "done",
            Activity::Failed => "failed",
            Activity::Interactive => "human",
            Activity::Unknown => "-",
        }
    }

    /// One-cell ASCII fallback for a STATE column squeezed below two cells.
    pub fn ascii(self) -> &'static str {
        match self {
            Activity::Busy => "*",
            Activity::Waiting => "z",
            Activity::Working => "w",
            Activity::NeedsDecision => "?",
            Activity::Blocked => "!",
            Activity::Paused => "~",
            Activity::Done => "+",
            Activity::Failed => "x",
            Activity::Interactive => "@",
            Activity::Unknown => ".",
        }
    }
}

/// Fit an activity into a STATE column of `width` display cells: the emoji at
/// two or more, the ASCII fallback at one, empty below that.
pub fn fit_state(activity: Activity, width: usize) -> String {
    match width {
        0 => String::new(),
        1 => activity.ascii().to_string(),
        _ => activity.glyph().to_string(),
    }
}

/// Last status verb of a `.status` log: the first whitespace-token before the
/// first `:` of the last non-empty line (`needs-decision [key=x]: ...` →
/// `needs-decision`). `None` when no line parses.
pub fn parse_status_verb(content: &str) -> Option<String> {
    let line = content.lines().rev().find(|l| !l.trim().is_empty())?;
    let head = line.split(':').next()?;
    let verb = head.split_whitespace().next()?;
    Some(verb.to_string())
}

/// Parse a firstmate busy-state record against its armed gen. Returns the
/// turn state only for a well-formed `v1` record whose gen matches;
/// everything else — missing gen, version drift, stale gen, unknown state
/// token, extra lines — is `None` (unknown), mirroring fm-busy-lib.sh's
/// "never idle on bad data" rule.
pub fn parse_busy_state(record: &str, armed_gen: Option<&str>) -> Option<bool> {
    let armed = armed_gen?.trim();
    if armed.is_empty() {
        return None;
    }
    let mut lines = record.lines().filter(|l| !l.trim().is_empty());
    let line = lines.next()?;
    if lines.next().is_some() {
        return None; // the contract is exactly one line
    }
    let mut fields = line.split_whitespace();
    if fields.next() != Some("v1") {
        return None;
    }
    let mut gen = None;
    let mut state = None;
    for f in fields {
        if let Some(v) = f.strip_prefix("gen=") {
            gen = Some(v);
        } else if let Some(v) = f.strip_prefix("state=") {
            state = Some(v);
        }
    }
    if gen? != armed {
        return None;
    }
    match state? {
        "busy" => Some(true),
        "idle" => Some(false),
        _ => None,
    }
}

/// Combine lifecycle verb and turn state into an [`Activity`] for a
/// fleet-matched row. Lifecycle beats turn state; unknown verbs degrade to
/// the working family rather than hiding the row's signal.
pub fn classify(verb: Option<&str>, busy: Option<bool>) -> Activity {
    match verb {
        Some("failed") => Activity::Failed,
        Some("done") => Activity::Done,
        Some("blocked") => Activity::Blocked,
        Some("needs-decision") => Activity::NeedsDecision,
        Some("paused") => Activity::Paused,
        _ => match busy {
            Some(true) => Activity::Busy,
            Some(false) => Activity::Waiting,
            None => Activity::Working,
        },
    }
}

/// Read and classify the activity for one fleet task under a firstmate
/// home's `state/` dir, keyed by the task's meta filename stem. Best-effort:
/// any unreadable file degrades that signal, never errors.
pub fn load_activity(fm_home: &Path, stem: &str) -> Activity {
    let state = fm_home.join("state");
    let status = fs::read_to_string(state.join(format!("{stem}.status"))).ok();
    let verb = status.as_deref().and_then(parse_status_verb);
    let record = fs::read_to_string(state.join(format!("{stem}.busy-state"))).ok();
    let gen = fs::read_to_string(state.join(format!("{stem}.busy-gen"))).ok();
    let busy = match (record.as_deref(), gen.as_deref()) {
        (Some(r), g) => parse_busy_state(r, g.map(str::trim)),
        _ => None,
    };
    classify(verb.as_deref(), busy)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEN: &str = "g1786790701.2685732.1306";

    fn record(state: &str) -> String {
        format!("v1 gen={GEN} seq=2 state={state} source=claude-hook event=stop ts=1786790704\n")
    }

    // --- glyph invariants ---

    #[test]
    fn glyphs_are_single_wide_scalars() {
        // Every glyph must be ONE scalar (no VS16/ZWJ sequences) so that
        // unicode-width 0.1, unicode-width 0.2, and terminal wcwidth all
        // agree it occupies exactly two cells. See the module header.
        let all = [
            Activity::Busy,
            Activity::Waiting,
            Activity::Working,
            Activity::NeedsDecision,
            Activity::Blocked,
            Activity::Paused,
            Activity::Done,
            Activity::Failed,
            Activity::Interactive,
            Activity::Unknown,
        ];
        for a in all {
            let g = a.glyph();
            assert_eq!(g.chars().count(), 1, "{a:?} glyph must be one scalar");
            let c = g.chars().next().unwrap();
            assert!(
                !matches!(c, '\u{FE0F}' | '\u{200D}'),
                "{a:?} glyph must not be a presentation/joiner char"
            );
            assert_eq!(a.ascii().chars().count(), 1);
            assert!(a.ascii().is_ascii());
        }
    }

    #[test]
    fn fit_state_degrades_emoji_ascii_empty() {
        assert_eq!(fit_state(Activity::Busy, 2), "🔨");
        assert_eq!(fit_state(Activity::Busy, 5), "🔨");
        assert_eq!(fit_state(Activity::Busy, 1), "*");
        assert_eq!(fit_state(Activity::Busy, 0), "");
    }

    // --- status verb parsing ---

    #[test]
    fn status_verb_takes_last_line() {
        let log = "working: setting up\nworking: building\ndone: all green\n";
        assert_eq!(parse_status_verb(log).as_deref(), Some("done"));
    }

    #[test]
    fn status_verb_strips_decision_key() {
        let log = "working: x\nneeds-decision [key=api-shape]: REST or RPC?\n";
        assert_eq!(parse_status_verb(log).as_deref(), Some("needs-decision"));
    }

    #[test]
    fn status_verb_empty_is_none() {
        assert_eq!(parse_status_verb(""), None);
        assert_eq!(parse_status_verb("\n\n"), None);
    }

    // --- busy-state parsing ---

    #[test]
    fn busy_record_matching_gen_parses() {
        assert_eq!(parse_busy_state(&record("busy"), Some(GEN)), Some(true));
        assert_eq!(parse_busy_state(&record("idle"), Some(GEN)), Some(false));
    }

    #[test]
    fn busy_record_unknown_state_is_none() {
        assert_eq!(parse_busy_state(&record("unknown"), Some(GEN)), None);
        assert_eq!(parse_busy_state(&record("bogus"), Some(GEN)), None);
    }

    #[test]
    fn busy_record_stale_gen_is_none() {
        // A record from a previous incarnation must never classify.
        assert_eq!(parse_busy_state(&record("busy"), Some("g999.1.1")), None);
    }

    #[test]
    fn busy_record_missing_gen_or_bad_version_is_none() {
        assert_eq!(parse_busy_state(&record("busy"), None), None);
        assert_eq!(parse_busy_state(&record("busy"), Some("")), None);
        let v2 = record("busy").replace("v1 ", "v2 ");
        assert_eq!(parse_busy_state(&v2, Some(GEN)), None);
    }

    #[test]
    fn busy_record_multiline_or_garbage_is_none() {
        let two = format!("{}{}", record("busy"), record("busy"));
        assert_eq!(parse_busy_state(&two, Some(GEN)), None);
        assert_eq!(parse_busy_state("not a record", Some(GEN)), None);
    }

    // --- classification ---

    #[test]
    fn lifecycle_beats_turn_state() {
        assert_eq!(classify(Some("done"), Some(true)), Activity::Done);
        assert_eq!(classify(Some("failed"), Some(false)), Activity::Failed);
        assert_eq!(classify(Some("blocked"), Some(true)), Activity::Blocked);
        assert_eq!(
            classify(Some("needs-decision"), Some(true)),
            Activity::NeedsDecision
        );
        assert_eq!(classify(Some("paused"), Some(true)), Activity::Paused);
    }

    #[test]
    fn working_splits_on_turn_state() {
        assert_eq!(classify(Some("working"), Some(true)), Activity::Busy);
        assert_eq!(classify(Some("working"), Some(false)), Activity::Waiting);
        assert_eq!(classify(Some("working"), None), Activity::Working);
        // resolved / unknown verbs / missing status degrade the same way.
        assert_eq!(classify(Some("resolved"), Some(true)), Activity::Busy);
        assert_eq!(classify(None, Some(false)), Activity::Waiting);
        assert_eq!(classify(None, None), Activity::Working);
    }
}
