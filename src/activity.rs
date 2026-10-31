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

use serde::Serialize;

/// What an agent row is doing right now, as rendered in the STATE column.
/// Serializes as its kebab-case variant name (`busy`, `needs-decision`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
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
    parse_status_line(content).map(|(verb, _)| verb)
}

/// Last status verb of a `.status` log plus the line's `[at=<epoch>]` stamp
/// (`paused [at=1793381955]: ...` → `("paused", Some(1793381955))`). The
/// verb is read as [`parse_status_verb`] reads it; the stamp as
/// [`parse_status_at`] does.
pub fn parse_status_line(content: &str) -> Option<(String, Option<u64>)> {
    let line = content.lines().rev().find(|l| !l.trim().is_empty())?;
    let head = line.split(':').next()?;
    let verb = head.split_whitespace().next()?.to_string();
    Some((verb, parse_status_at(line)))
}

/// The `[at=<epoch>]` emission stamp of one status line, ported from the
/// grammar's owner, firstmate's `_fm_status_at_epoch` in
/// `bin/fm-classify-lib.sh`: the stamp sits in the head before the line's
/// first `:` (a line without one has none), runs from the first `[at=` to the
/// next `]`, and is canonical unsigned decimal of at most 12 digits (no
/// leading zero). A malformed or duplicated stamp means unknown time: `None`.
pub fn parse_status_at(line: &str) -> Option<u64> {
    let (head, _) = line.split_once(':')?;
    let rest = &head[head.find("[at=")? + "[at=".len()..];
    let (value, after) = rest.split_once(']')?;
    if after.contains("[at=")
        || value.is_empty()
        || value.len() > 12
        || !value.bytes().all(|b| b.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

/// One line of a task's `.status` log, read as firstmate's classifier
/// (`bin/fm-classify-lib.sh`) reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusLine {
    /// The line's verb (`working`, `needs-decision`, `resolved`, ...), ported
    /// from firstmate's `status_line_verb`: the words before the first colon
    /// and the first `[` tag, with the `corr=<16 hex>` tokens firstmate's
    /// own tooling writes after the first word dropped. Anything else stays,
    /// so a prose head (`working on it: ...`) is not a verb any reader
    /// should act on.
    pub verb: String,
    /// The `[at=<epoch>]` stamp, as [`parse_status_at`] reads it.
    pub at: Option<u64>,
    /// The stated decision key: a complete `[key=<slug>]` token before the
    /// line's first colon, else one at the head of the text after it. A slug
    /// outside `A-Za-z0-9._-` is `invalid-key`, the bucket firstmate files it
    /// under; no token is `None` (firstmate's `default` key).
    pub key: Option<String>,
    /// The text after the line's first colon, leading blanks trimmed, with a
    /// note-head key token removed when it is the line's key; the whole line
    /// when it has no colon.
    pub text: String,
}

/// Every non-empty line of a `.status` log, oldest first. A line without a
/// verb (only blanks before its colon) is skipped.
pub fn parse_status_log(content: &str) -> Vec<StatusLine> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_status_log_line)
        .collect()
}

/// One status line as a [`StatusLine`]; `None` when it has no verb.
pub fn parse_status_log_line(line: &str) -> Option<StatusLine> {
    let verb = line_verb(line)?;
    let unstamped = unstamped(line);
    let (key, text) = match unstamped.split_once(':') {
        None => (key_before_colon(&unstamped), unstamped.clone()),
        Some((head, rest)) => {
            let note = rest.trim_start();
            match key_before_colon(head) {
                Some(key) => (Some(key), note.to_string()),
                None => match note_head_key(note) {
                    Some((raw, after)) if slug_ok(raw) => {
                        (Some(raw.to_string()), after.trim_start().to_string())
                    }
                    Some(_) => (Some(INVALID_KEY.to_string()), note.to_string()),
                    None => (None, note.to_string()),
                },
            }
        }
    };
    Some(StatusLine {
        verb,
        at: parse_status_at(line),
        key,
        text,
    })
}

/// A status line's verb (see [`StatusLine::verb`]); `None` when the head is
/// blank.
fn line_verb(line: &str) -> Option<String> {
    let head = line.split(':').next()?.split('[').next()?;
    let mut words = head.split_whitespace();
    let mut verb = words.next()?.to_string();
    for word in words {
        if !is_corr_token(word) {
            verb.push(' ');
            verb.push_str(word);
        }
    }
    Some(verb)
}

/// `corr=` and exactly 16 hex digits: the unbracketed correlation token
/// firstmate's pending-reply library writes (`_fm_classify_is_corr_token`).
fn is_corr_token(word: &str) -> bool {
    word.strip_prefix("corr=")
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The key bucket firstmate files a stated key with a malformed slug under.
const INVALID_KEY: &str = "invalid-key";

/// The line with its `[at=…]` stamps removed, ported from firstmate's
/// `_fm_status_unstamped`: each stamp whose prefix holds no colon goes,
/// with one blank before it, so a stamp like `[at=10:30]` cannot move the
/// head/note separator.
fn unstamped(line: &str) -> String {
    let mut keep = String::new();
    let mut rest = line;
    while let Some(start) = rest.find("[at=") {
        let Some(close) = rest[start..].find(']') else {
            break;
        };
        let before = &rest[..start];
        if before.contains(':') {
            break;
        }
        keep.push_str(before.strip_suffix(' ').unwrap_or(before));
        rest = &rest[start + close + 1..];
    }
    keep.push_str(rest);
    keep
}

/// The key of a complete `[key=…]` token in a line's head (the part before
/// its first colon).
fn key_before_colon(head: &str) -> Option<String> {
    let rest = &head[head.find("[key=")? + "[key=".len()..];
    let (raw, _) = rest.split_once(']')?;
    Some(if slug_ok(raw) {
        raw.to_string()
    } else {
        INVALID_KEY.to_string()
    })
}

/// A complete `[key=…]` token at the very head of a note: its raw slug and
/// the text after it.
fn note_head_key(note: &str) -> Option<(&str, &str)> {
    note.strip_prefix("[key=")?.split_once(']')
}

fn slug_ok(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A well-formed, gen-valid firstmate busy-state record: the turn state
/// (`None` for the contract's `unknown`) and its `ts=` stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyRecord {
    pub busy: Option<bool>,
    pub ts: u64,
}

/// Parse a firstmate busy-state record against its armed gen. Returns the
/// turn state only for a well-formed `v1` record whose gen matches;
/// everything else — missing gen, version drift, stale gen, malformed
/// record — is `None` (unknown), mirroring fm-busy-lib.sh's "never idle on
/// bad data" rule.
pub fn parse_busy_state(record: &str, armed_gen: Option<&str>) -> Option<bool> {
    parse_busy_record(record, armed_gen)?.busy
}

/// Parse a firstmate busy-state record against its armed gen, keeping its
/// `ts=` stamp. Ported from the grammar's owner, `fm_busy_record_read` in
/// firstmate's `bin/fm-busy-lib.sh`: exactly one newline-terminated line of
/// space-separated `v1` then only the `gen=`, `seq=`, `state=`, `source=`,
/// `event=` and `ts=` fields; `gen`, `source` and `event` are
/// `[A-Za-z0-9._-]+` tokens, `seq` and `ts` are digits, `state` is `busy`,
/// `idle` or `unknown`. A record that breaks any rule, or whose gen is not the
/// armed one, is `None`.
pub fn parse_busy_record(record: &str, armed_gen: Option<&str>) -> Option<BusyRecord> {
    let armed = armed_gen?.trim();
    if armed.is_empty() {
        return None;
    }
    let line = record.strip_suffix('\n')?;
    if line.contains('\n') {
        return None; // the contract is exactly one line
    }
    let mut fields = line.split(' ').filter(|f| !f.is_empty());
    if fields.next() != Some("v1") {
        return None;
    }
    let (mut gen, mut seq, mut state, mut source, mut event, mut ts) = ("", "", "", "", "", "");
    for f in fields {
        let (key, value) = f.split_once('=')?;
        match key {
            "gen" => gen = value,
            "seq" => seq = value,
            "state" => state = value,
            "source" => source = value,
            "event" => event = value,
            "ts" => ts = value,
            _ => return None,
        }
    }
    let token = |v: &str| {
        !v.is_empty()
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    let digits = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit());
    if !token(gen) || !token(source) || !token(event) || !digits(seq) || !digits(ts) {
        return None;
    }
    let busy = match state {
        "busy" => Some(true),
        "idle" => Some(false),
        "unknown" => None,
        _ => return None,
    };
    if gen != armed {
        return None;
    }
    Some(BusyRecord {
        busy,
        ts: ts.parse().ok()?,
    })
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

/// One fleet task's classified [`Activity`] plus the raw signals behind it,
/// which a caller can use to judge staleness.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskState {
    pub activity: Activity,
    /// Verb of the last `.status` line.
    pub status_verb: Option<String>,
    /// `[at=<epoch>]` stamp of the last `.status` line.
    pub status_at: Option<u64>,
    /// `ts=` stamp of the gen-valid busy-state record.
    pub busy_ts: Option<u64>,
    /// Every line of the `.status` log, oldest first.
    pub status_log: Vec<StatusLine>,
}

/// Read and classify the activity for one fleet task under a firstmate
/// home's `state/` dir, keyed by the task's meta filename stem. Best-effort:
/// any unreadable file degrades that signal, never errors.
pub fn load_activity(fm_home: &Path, stem: &str) -> Activity {
    load_task_state(fm_home, stem).activity
}

/// [`load_activity`] keeping the status and busy-state stamps it read, and
/// the whole status log.
pub fn load_task_state(fm_home: &Path, stem: &str) -> TaskState {
    let state = fm_home.join("state");
    let status = fs::read_to_string(state.join(format!("{stem}.status"))).ok();
    let (status_verb, status_at) = match status.as_deref().and_then(parse_status_line) {
        Some((verb, at)) => (Some(verb), at),
        None => (None, None),
    };
    let record = fs::read_to_string(state.join(format!("{stem}.busy-state"))).ok();
    let gen = fs::read_to_string(state.join(format!("{stem}.busy-gen"))).ok();
    let busy = match (record.as_deref(), gen.as_deref()) {
        (Some(r), g) => parse_busy_record(r, g.map(str::trim)),
        _ => None,
    };
    TaskState {
        activity: classify(status_verb.as_deref(), busy.and_then(|b| b.busy)),
        status_verb,
        status_at,
        busy_ts: busy.map(|b| b.ts),
        status_log: status.as_deref().map(parse_status_log).unwrap_or_default(),
    }
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

    #[test]
    fn status_line_reads_the_at_stamp_beside_other_tokens() {
        let log = "working [at=1793381000]: setup\n\
                   needs-decision [at=1793381955] [key=api-shape]: REST or RPC?\n";
        assert_eq!(
            parse_status_line(log),
            Some(("needs-decision".to_string(), Some(1_793_381_955)))
        );
        let resolved = "resolved [key=api-shape] [at=1793382000]: RPC\n";
        assert_eq!(
            parse_status_line(resolved),
            Some(("resolved".to_string(), Some(1_793_382_000)))
        );
        // firstmate matches the stamp as a substring, not a whole token.
        assert_eq!(
            parse_status_at("working [at=1793381955][key=x]: glued"),
            Some(1_793_381_955)
        );
        // A lone zero is canonical decimal; twelve digits is the ceiling.
        assert_eq!(parse_status_at("working [at=0]: zero"), Some(0));
        assert_eq!(
            parse_status_at("working [at=999999999999]: max"),
            Some(999_999_999_999)
        );
    }

    #[test]
    fn status_line_without_a_plain_digit_stamp_has_no_time() {
        for log in [
            "working: no stamp\n",
            "working [at=<epoch>]: unsubstituted\n",
            "working [at=]: empty\n",
            "working [at=-5]: signed\n",
            "working [at=+5]: plus\n",
            // Duplicate stamps mean unknown time, even when they agree.
            "working [at=1] [at=2]: dup\n",
            "working [at=7] [at=7]: same\n",
            "working [at=0123]: leading zero\n",
            "working [at=1234567890123]: thirteen digits\n",
            "working [at=17933 81955]: space\n",
            "working [at=1793381955: unclosed\n",
            // No colon: firstmate reads no stamp from such a line.
            "working [at=1793381955]\n",
        ] {
            assert_eq!(
                parse_status_line(log),
                Some(("working".to_string(), None)),
                "{log:?}"
            );
        }
        // A stamp in the message body (after the first colon) is not the line's.
        assert_eq!(
            parse_status_line("done: was [at=1793381955]\n"),
            Some(("done".to_string(), None))
        );
    }

    // --- the whole status log ---

    fn line(verb: &str, at: Option<u64>, key: Option<&str>, text: &str) -> StatusLine {
        StatusLine {
            verb: verb.to_string(),
            at,
            key: key.map(str::to_string),
            text: text.to_string(),
        }
    }

    #[test]
    fn status_log_reads_every_line_oldest_first() {
        let log = "working [at=1793434552]: setup done\n\
                   \n\
                   paused [at=1793435067]: awaiting pre-push review of fm/x at a5416b7\n\
                   needs-decision [at=1793435100] [key=api-shape]: REST or RPC?\n\
                   resolved [key=api-shape] [at=1793435200]: RPC\n\
                   done [at=1793436166]: PR https://github.com/o/r/pull/51\n";
        assert_eq!(
            parse_status_log(log),
            vec![
                line("working", Some(1_793_434_552), None, "setup done"),
                line(
                    "paused",
                    Some(1_793_435_067),
                    None,
                    "awaiting pre-push review of fm/x at a5416b7"
                ),
                line(
                    "needs-decision",
                    Some(1_793_435_100),
                    Some("api-shape"),
                    "REST or RPC?"
                ),
                line("resolved", Some(1_793_435_200), Some("api-shape"), "RPC"),
                line(
                    "done",
                    Some(1_793_436_166),
                    None,
                    "PR https://github.com/o/r/pull/51"
                ),
            ]
        );
        assert!(parse_status_log("").is_empty());
        assert!(parse_status_log(": no verb\n  \n").is_empty());
    }

    #[test]
    fn status_log_keys_follow_firstmates_two_positions() {
        // The documented position, and a complete token at the note's head.
        let at_head = parse_status_log_line("needs-decision: [key=k1] which one?").unwrap();
        assert_eq!(at_head.key.as_deref(), Some("k1"));
        assert_eq!(at_head.text, "which one?");
        // Both: the before-colon token wins and the note keeps its own.
        let both = parse_status_log_line("needs-decision [key=a]: [key=b] x").unwrap();
        assert_eq!(both.key.as_deref(), Some("a"));
        assert_eq!(both.text, "[key=b] x");
        // A token deeper in the note is prose, not a key.
        let prose = parse_status_log_line("resolved: answered, see [key=x]").unwrap();
        assert_eq!(prose.key, None);
        // A malformed slug folds under firstmate's invalid-key bucket.
        let bad = parse_status_log_line("needs-decision [key=a b]: x").unwrap();
        assert_eq!(bad.key.as_deref(), Some("invalid-key"));
        let bad_head = parse_status_log_line("needs-decision: [key=a/b] x").unwrap();
        assert_eq!(bad_head.key.as_deref(), Some("invalid-key"));
        assert_eq!(bad_head.text, "[key=a/b] x");
        // An unclosed token is no token.
        let open = parse_status_log_line("needs-decision [key=abc: x").unwrap();
        assert_eq!(open.key, None);
    }

    #[test]
    fn status_log_text_survives_stamps_with_colons_and_lines_without_one() {
        // A readable stamp's colons do not end the head early.
        let l = parse_status_log_line("working [at=10:30] [key=k]: note").unwrap();
        assert_eq!((l.verb.as_str(), l.at), ("working", None));
        assert_eq!(l.key.as_deref(), Some("k"));
        assert_eq!(l.text, "note");
        // A stamp after the separator is note text, kept as written.
        let l = parse_status_log_line("done [at=5]: merged [at=10:30] at noon").unwrap();
        assert_eq!(
            (l.at, l.text.as_str()),
            (Some(5), "merged [at=10:30] at noon")
        );
        let l = parse_status_log_line("done: merged [at=10:30] at noon").unwrap();
        assert_eq!(l.text, "merged [at=10:30] at noon");
        // No colon: the whole (unstamped) line is the text.
        let l = parse_status_log_line("working [at=1793381955] still going").unwrap();
        assert_eq!(l.verb, "working");
        assert_eq!(l.at, None); // firstmate reads no stamp without a colon
        assert_eq!(l.text, "working still going");
    }

    #[test]
    fn status_log_verbs_drop_only_correlation_tokens() {
        let l = parse_status_log_line(
            "needs-decision corr=0123456789abcdef [key=texte]: which wall text?",
        )
        .unwrap();
        assert_eq!(l.verb, "needs-decision");
        assert_eq!(l.key.as_deref(), Some("texte"));
        // A wrong-length token, or prose, stays part of the head: not a verb.
        assert_eq!(
            parse_status_log_line("resolved corr=0123 [key=k]: x")
                .unwrap()
                .verb,
            "resolved corr=0123"
        );
        assert_eq!(
            parse_status_log_line("working on it: x").unwrap().verb,
            "working on it"
        );
        assert_eq!(
            parse_status_log_line("working[key=k]: glued").unwrap().verb,
            "working"
        );
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
    fn busy_record_keeps_its_ts_and_survives_an_unknown_state() {
        assert_eq!(
            parse_busy_record(&record("busy"), Some(GEN)),
            Some(BusyRecord {
                busy: Some(true),
                ts: 1_786_790_704
            })
        );
        assert_eq!(
            parse_busy_record(&record("unknown"), Some(GEN)),
            Some(BusyRecord {
                busy: None,
                ts: 1_786_790_704
            })
        );
        // A stale incarnation's stamp is not this task's.
        assert_eq!(parse_busy_record(&record("busy"), Some("g999.1.1")), None);
    }

    #[test]
    fn task_state_reads_status_and_busy_stamps() {
        let home = std::env::temp_dir().join(format!(
            "crew-watch-activity-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state = home.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("t.status"), "working [at=1786790700]: go\n").unwrap();
        fs::write(state.join("t.busy-state"), record("idle")).unwrap();
        fs::write(state.join("t.busy-gen"), format!("{GEN}\n")).unwrap();
        assert_eq!(
            load_task_state(&home, "t"),
            TaskState {
                activity: Activity::Waiting,
                status_verb: Some("working".to_string()),
                status_at: Some(1_786_790_700),
                busy_ts: Some(1_786_790_704),
                status_log: vec![line("working", Some(1_786_790_700), None, "go")],
            }
        );
        // Nothing on disk: the working-no-signal state and no stamps.
        assert_eq!(
            load_task_state(&home, "absent"),
            TaskState {
                activity: Activity::Working,
                ..TaskState::default()
            }
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn activity_serializes_as_its_kebab_case_name() {
        assert_eq!(
            serde_json::to_string(&Activity::NeedsDecision).unwrap(),
            "\"needs-decision\""
        );
        assert_eq!(
            serde_json::to_string(&Activity::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn busy_record_rejects_what_firstmate_calls_malformed() {
        let ok = record("busy");
        for bad in [
            ok.replace("ts=1786790704", "ts=+5"),
            ok.replace("ts=1786790704", "ts=abc"),
            ok.replace("ts=1786790704", "ts="),
            ok.replace(" ts=1786790704", ""),
            ok.replace("state=busy", "state=garbage"),
            ok.replace("seq=2", "seq=x"),
            ok.replace("source=claude-hook", "source=claude/hook"),
            ok.replace("event=stop", "event="),
            ok.replace(" event=stop", " event=stop extra=1"),
            ok.replace(" event=stop", " event=stop bare"),
            ok.trim_end().to_string(), // no terminating newline
            format!("{ok}\n"),         // a second, empty line
        ] {
            assert_eq!(parse_busy_record(&bad, Some(GEN)), None, "{bad:?}");
        }
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
