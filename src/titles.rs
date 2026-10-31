//! Human task-title lookup from the firstmate home.
//!
//! Resolves a task id to a short human title, so the TASK column reads like a
//! sentence rather than an opaque id. Two best-effort sources, consulted in
//! order; both are pure parsers so they can be unit-tested with fixture data:
//!
//! 1. **`data/backlog.md`** — one markdown checkbox per task id:
//!    `- [ ] <task-id> - <title> (repo: ...) (kind: ...) (since ...)`.
//!    The title is the text between ` - <task-id> - ` and the first trailing
//!    marker (`(repo:`, `(kind:`, `(since`, `(done`, `blocked-by:`).
//! 2. **`data/<task-id>/brief.md`** — fallback: the first sentence of the first
//!    paragraph under the `# Task` heading.
//!
//! Everything is read-only and non-fatal: a missing/unreadable home or file
//! never propagates as an error; the caller simply falls back.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Map of task id -> human title, loaded best-effort from the firstmate home.
#[derive(Debug, Clone, Default)]
pub struct TaskTitles(HashMap<String, String>);

impl TaskTitles {
    /// Look up the title for a task id.
    pub fn title_for(&self, task_id: &str) -> Option<&str> {
        self.0.get(task_id).map(|s| s.as_str())
    }

    /// Whether there are no entries.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Build a lookup from `(task_id, title)` pairs. For tests; production code
    /// uses [`load_task_titles`].
    #[cfg(test)]
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        TaskTitles(pairs.into_iter().collect())
    }
}

/// Load task titles from a firstmate home. Never fails: a missing dir or
/// unreadable file is silently skipped. Backlog titles win over brief titles.
pub fn load_task_titles(home: &Path) -> TaskTitles {
    load_task_titles_with(home, &load_backlog_entries(home))
}

/// [`load_task_titles`] with the backlog already read, so a caller that also
/// wants the [`BacklogEntry`]s reads `data/backlog.md` once.
pub fn load_task_titles_with(home: &Path, backlog: &[BacklogEntry]) -> TaskTitles {
    let mut map: HashMap<String, String> = HashMap::new();

    // Layer 2 (lower priority): brief.md first sentences.
    if let Ok(entries) = fs::read_dir(home.join("data")) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(task_id) = dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let brief = dir.join("brief.md");
            if let Ok(content) = fs::read_to_string(&brief) {
                if let Some(title) = extract_brief_title(&content) {
                    map.entry(task_id.to_string()).or_insert(title);
                }
            }
        }
    }

    // Layer 1 (higher priority): backlog titles overwrite brief titles.
    for entry in backlog {
        map.insert(entry.id.clone(), entry.title.clone());
    }

    TaskTitles(map)
}

/// Parse `data/backlog.md` into `(task_id, title)` pairs. Lenient: any line
/// that is not a `- [ ] <id> - ...` / `- [x] <id> - ...` checkbox is skipped.
pub fn parse_backlog(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(checkbox)
        .map(|(id, title, _)| (id, title))
        .collect()
}

/// One task of `data/backlog.md`: its checkbox line's id, title and
/// `(name: value)` markers, and the indented note lines under it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct BacklogEntry {
    pub id: String,
    /// The title, as [`parse_backlog`] reads it.
    pub title: String,
    /// The checkbox is ticked (`- [x]`).
    pub done: bool,
    /// The `(kind: …)` marker: `ship`, `scout`, `captain`, ...
    pub kind: Option<String>,
    /// The `(repo: …)` marker.
    pub repo: Option<String>,
    /// The `(hold-kind: …)` marker of a held task (`captain`, ...).
    pub hold_kind: Option<String>,
    /// The checkbox line itself.
    pub line: String,
    /// The indented lines under the checkbox, up to the next checkbox,
    /// heading or blank line, each trimmed, joined with newlines.
    pub note: String,
}

impl BacklogEntry {
    /// The planner's estimate in minutes: the first `estimate=<N>m` token in
    /// the entry (checkbox line, then note), as firstmate's effort ledger
    /// (`bin/fm-effort-ledger.sh`) reads it: the token starts the text or
    /// follows a character that is not a letter, digit, `_` or `-`.
    pub fn estimate_min(&self) -> Option<u64> {
        [self.line.as_str(), self.note.as_str()]
            .into_iter()
            .find_map(first_estimate)
    }
}

fn first_estimate(text: &str) -> Option<u64> {
    const TOKEN: &str = "estimate=";
    let mut from = 0;
    while let Some(i) = text[from..].find(TOKEN).map(|i| i + from) {
        from = i + TOKEN.len();
        let starts_word = text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        let tail = &text[from..];
        let n = tail.bytes().take_while(u8::is_ascii_digit).count();
        if starts_word && n > 0 && tail[n..].starts_with('m') {
            if let Ok(minutes) = tail[..n].parse() {
                return Some(minutes);
            }
        }
    }
    None
}

/// Parse `data/backlog.md` into its entries, in file order. Lenient in the
/// same way as [`parse_backlog`]: a line that is not a task checkbox is no
/// entry (a heading or blank line ends the note of the one before it).
pub fn parse_backlog_entries(content: &str) -> Vec<BacklogEntry> {
    let mut out: Vec<BacklogEntry> = Vec::new();
    let mut in_note = false;
    for line in content.lines() {
        if let Some((id, title, done)) = checkbox(line) {
            out.push(BacklogEntry {
                id,
                title,
                done,
                kind: marker(line, "kind"),
                repo: marker(line, "repo"),
                hold_kind: marker(line, "hold-kind"),
                line: line.trim().to_string(),
                note: String::new(),
            });
            in_note = true;
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !in_note || !indented || line.trim().is_empty() {
            in_note = false;
            continue;
        }
        if let Some(entry) = out.last_mut() {
            if !entry.note.is_empty() {
                entry.note.push('\n');
            }
            entry.note.push_str(line.trim());
        }
    }
    out
}

/// Every backlog entry under a firstmate home; empty when the backlog is
/// missing or unreadable.
pub fn load_backlog_entries(home: &Path) -> Vec<BacklogEntry> {
    fs::read_to_string(home.join("data").join("backlog.md"))
        .map(|c| parse_backlog_entries(&c))
        .unwrap_or_default()
}

/// A task checkbox line's id, title and tick, as [`parse_backlog`] reads it.
fn checkbox(line: &str) -> Option<(String, String, bool)> {
    let s = line.trim_start().strip_prefix("- [")?;
    let mark = s.chars().next()?;
    if !matches!(mark, ' ' | 'x' | 'X') {
        return None;
    }
    let rest = s[1..].strip_prefix("] ")?;
    let (id, title_part) = rest.split_once(" - ")?;
    let title = strip_trailing_markers(title_part.trim());
    (!title.is_empty()).then(|| (id.trim().to_string(), title, mark != ' '))
}

/// The value of a `(name: value)` marker on a checkbox line, read as firstmate's
/// own backlog reader (`bin/fm-fleet-snapshot.sh`, `metadata`) reads it: the
/// **last** `name:` right after a `(`, or after a `,` and any blanks; its value
/// runs to the next `,` or `)` (or the line's end), trimmed. Empty is none.
fn marker(line: &str, name: &str) -> Option<String> {
    let key = format!("{name}:");
    let at = line.rmatch_indices(&key).map(|(i, _)| i).find(|&i| {
        let before = &line[..i];
        before.ends_with('(') || before.trim_end().ends_with(',')
    })?;
    let rest = &line[at + key.len()..];
    let value = rest[..rest.find([',', ')']).unwrap_or(rest.len())].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Strip the trailing firstmate metadata markers from a backlog title:
/// ` (repo: ...)`, ` (kind: ...)`, ` (since ...)`, ` (done ...)`, and
/// ` blocked-by: ...`. The title itself may contain parenthesized text, so we
/// only cut at the first marker keyword, not at every `(`.
fn strip_trailing_markers(title: &str) -> String {
    const MARKERS: &[&str] = &[" (repo:", " (kind:", " (since", " (done", " blocked-by:"];
    let mut cut = title.len();
    for &m in MARKERS {
        if let Some(idx) = title.find(m) {
            if idx < cut {
                cut = idx;
            }
        }
    }
    title[..cut].trim_end().to_string()
}

/// Extract the first sentence of the first paragraph under the `# Task` heading
/// in a brief.md. Returns `None` if the heading or paragraph is absent.
pub fn extract_brief_title(content: &str) -> Option<String> {
    let mut lines = content.lines();
    // Find the `# Task` heading.
    let mut found = false;
    for line in lines.by_ref() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("# task") || t.eq_ignore_ascii_case("#task") {
            found = true;
            break;
        }
    }
    if !found {
        return None;
    }

    // Collect the first non-empty paragraph after the heading.
    let mut paragraph: Vec<&str> = Vec::new();
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        // Stop at the next markdown heading.
        if line.trim_start().starts_with('#') {
            break;
        }
        paragraph.push(line);
        // One line is enough for a title.
        break;
    }

    let line = paragraph.first()?.trim();
    if line.is_empty() {
        return None;
    }
    Some(first_sentence(line))
}

/// Take the first sentence of a line: everything up to the first `. ` (period
/// followed by space) or end of line. A period with no trailing space (e.g.
/// `v1.0.0`) is not treated as a sentence end.
fn first_sentence(line: &str) -> String {
    // Find ". " as a sentence boundary.
    if let Some(idx) = line.find(". ") {
        return line[..=idx].trim().to_string();
    }
    // If the line ends with '.', keep it as the sentence.
    line.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- backlog entries ---

    const ENTRIES: &str = "\
# Backlog

## In flight
- [ ] miles-timeline-build - Step timelines on the Miles board (repo: miles) (kind: ship) (since 2026-10-31)
  Captain 2026-10-31: yes to all five. estimate=125m dispatch-facts at=2026-10-31T09:51:11+0000
- [ ] rv-miles-badge - OCR review round miles#56 at 7e2f5ba (kind: scout) (since 2026-10-31)
- [ ] held - a captain's call (repo: x) (kind: captain) (hold: fm-hold-v1:abc=) (hold-kind: captain)
  Captain hold set: 2026-10-24T05:44:11Z

  not a note: the blank line ended it
## Done
- [x] miles-images - Miles: pictures https://github.com/o/miles/pull/52 (repo: miles) (kind: ship) (merged 2026-10-31)
";

    #[test]
    fn backlog_entries_carry_markers_and_notes() {
        let entries = parse_backlog_entries(ENTRIES);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "miles-timeline-build",
                "rv-miles-badge",
                "held",
                "miles-images"
            ]
        );
        let build = &entries[0];
        assert_eq!(build.title, "Step timelines on the Miles board");
        assert!(!build.done);
        assert_eq!(build.kind.as_deref(), Some("ship"));
        assert_eq!(build.repo.as_deref(), Some("miles"));
        assert_eq!(build.hold_kind, None);
        assert!(build.note.starts_with("Captain 2026-10-31"));
        assert_eq!(build.estimate_min(), Some(125));
        assert_eq!(entries[1].kind.as_deref(), Some("scout"));
        assert_eq!(entries[1].repo, None);
        assert_eq!(entries[1].estimate_min(), None);
        let held = &entries[2];
        assert_eq!(held.hold_kind.as_deref(), Some("captain"));
        assert_eq!(held.note, "Captain hold set: 2026-10-24T05:44:11Z");
        assert!(entries[3].done);
        // The titles agree with the pair reader's.
        let pairs: Vec<(String, String)> = entries
            .iter()
            .map(|e| (e.id.clone(), e.title.clone()))
            .collect();
        assert_eq!(pairs, parse_backlog(ENTRIES));
    }

    #[test]
    fn markers_are_read_as_firstmates_backlog_reader_reads_them() {
        let m = |line: &str, name: &str| marker(line, name);
        // The last marker wins, so a title cannot impersonate one.
        assert_eq!(
            m("- [ ] t - Fix the (kind: ui) crash (kind: ship)", "kind").as_deref(),
            Some("ship")
        );
        // A comma ends the value, and may start another marker in the same group.
        assert_eq!(
            m("- [ ] t - x (kind: ship, repo: miles)", "kind").as_deref(),
            Some("ship")
        );
        assert_eq!(
            m("- [ ] t - x (kind: ship,repo: miles)", "repo").as_deref(),
            Some("miles")
        );
        // `hold-kind:` is not `kind:`.
        assert_eq!(m("- [ ] t - x (hold-kind: captain)", "kind"), None);
        assert_eq!(
            m("- [ ] t - x (kind: scout) (hold-kind: captain)", "kind").as_deref(),
            Some("scout")
        );
        assert_eq!(m("- [ ] t - x (kind: )", "kind"), None);
        assert_eq!(m("- [ ] t - x kind: ship", "kind"), None);
        assert_eq!(m("- [ ] t - x ( kind: ship)", "kind"), None);
        // An unclosed group runs to the end of the line.
        assert_eq!(
            m("- [ ] t - x (repo: miles", "repo").as_deref(),
            Some("miles")
        );
    }

    #[test]
    fn estimates_are_read_as_the_effort_ledger_reads_them() {
        let est = |line: &str| {
            BacklogEntry {
                line: line.to_string(),
                ..BacklogEntry::default()
            }
            .estimate_min()
        };
        assert_eq!(est("estimate=25m"), Some(25));
        assert_eq!(est("(x) estimate=40m, later"), Some(40));
        assert_eq!(est("estimate=5min"), Some(5)); // the token ends at the m
        for none in [
            "re-estimate=25m",
            "my_estimate=25m",
            "xestimate=25m",
            "estimate=m",
            "estimate=25",
            "estimate= 25m",
        ] {
            assert_eq!(est(none), None, "{none:?}");
        }
        // A token that does not count does not hide a later one.
        assert_eq!(est("re-estimate=9m then estimate=30m"), Some(30));
        // The checkbox line's token comes before the note's.
        let both = BacklogEntry {
            line: "- [ ] a - t estimate=10m".to_string(),
            note: "estimate=20m".to_string(),
            ..BacklogEntry::default()
        };
        assert_eq!(both.estimate_min(), Some(10));
    }

    // --- backlog parsing ---

    const BACKLOG_SAMPLE: &str = "\
# Backlog

## In flight
- [ ] crew-watch-model-task-cols - crew-watch: add MODEL column (parse from argv) and TASK title (repo: crew-watch) (kind: ship) (since 2026-08-11)
- [ ] fm-afk-resurface-loop - away mode unusable: resurface-after-rearm fires ~1/sec (repo: firstmate) (kind: ship) (since 2026-08-11)
  Some continuation paragraph on the next line.
- [x] crew-watch-v1 - build crew-watch v1: Rust TUI fleet monitor https://github.com/x/y (repo: crew-watch) (kind: ship) (done 2026-08-11)
- [ ] short-task - just a title with no markers
- [ ] edge - multi (parenthesized) title (kind: here)
- not a checkbox
- [ ] no-separator here
## Queued
";

    #[test]
    fn parse_backlog_extracts_titles() {
        let pairs = parse_backlog(BACKLOG_SAMPLE);
        let map: HashMap<String, String> = pairs.into_iter().collect();
        assert_eq!(
            map.get("crew-watch-model-task-cols").map(|s| s.as_str()),
            Some("crew-watch: add MODEL column (parse from argv) and TASK title")
        );
        assert_eq!(
            map.get("fm-afk-resurface-loop").map(|s| s.as_str()),
            Some("away mode unusable: resurface-after-rearm fires ~1/sec")
        );
    }

    #[test]
    fn parse_backlog_done_items_with_url() {
        let pairs = parse_backlog(BACKLOG_SAMPLE);
        let map: HashMap<String, String> = pairs.into_iter().collect();
        // URL is part of the title text (no marker keyword before it), but
        // (repo: ...) onward is stripped.
        assert_eq!(
            map.get("crew-watch-v1").map(|s| s.as_str()),
            Some("build crew-watch v1: Rust TUI fleet monitor https://github.com/x/y")
        );
    }

    #[test]
    fn parse_backlog_title_with_no_markers() {
        let pairs = parse_backlog(BACKLOG_SAMPLE);
        let map: HashMap<String, String> = pairs.into_iter().collect();
        assert_eq!(
            map.get("short-task").map(|s| s.as_str()),
            Some("just a title with no markers")
        );
    }

    #[test]
    fn parse_backlog_strips_only_trailing_marker_keywords() {
        // A title with an internal parenthesized group is preserved; only the
        // trailing (kind: ...) marker is stripped.
        let pairs = parse_backlog(BACKLOG_SAMPLE);
        let map: HashMap<String, String> = pairs.into_iter().collect();
        assert_eq!(
            map.get("edge").map(|s| s.as_str()),
            Some("multi (parenthesized) title")
        );
    }

    #[test]
    fn parse_backlog_skips_non_checkbox_lines() {
        let pairs = parse_backlog(BACKLOG_SAMPLE);
        let map: HashMap<String, String> = pairs.into_iter().collect();
        assert!(!map.contains_key("not"));
        assert!(!map.contains_key("no-separator"));
    }

    #[test]
    fn parse_backlog_handles_uppercase_checkbox_mark() {
        let content = "- [X] done-task - a finished thing (repo: x) (done 2026-01-01)";
        let pairs = parse_backlog(content);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "done-task");
        assert_eq!(pairs[0].1, "a finished thing");
    }

    #[test]
    fn parse_backlog_empty_is_empty() {
        assert!(parse_backlog("").is_empty());
    }

    #[test]
    fn parse_backlog_strips_blocked_by() {
        let content =
            "- [ ] blocked-task - needs a thing blocked-by: other-task (repo: x) (kind: y)";
        let pairs = parse_backlog(content);
        assert_eq!(pairs[0].1, "needs a thing");
    }

    // --- brief title extraction ---

    const BRIEF_SAMPLE: &str = "\
You are a crewmate.

# Task
Build v1 of crew-watch: a Rust TUI monitor for the captain's workstation. The repo is fresh and contains only a README, so you build from scratch.

## Why it exists
The captain uses htop.
";

    #[test]
    fn extract_brief_title_first_sentence() {
        let title = extract_brief_title(BRIEF_SAMPLE);
        assert_eq!(
            title.as_deref(),
            Some("Build v1 of crew-watch: a Rust TUI monitor for the captain's workstation.")
        );
    }

    #[test]
    fn extract_brief_title_skips_blank_lines_after_heading() {
        let content = "# Task\n\n\nFirst sentence here. More text.\n";
        assert_eq!(
            extract_brief_title(content).as_deref(),
            Some("First sentence here.")
        );
    }

    #[test]
    fn extract_brief_title_case_insensitive_heading() {
        assert_eq!(
            extract_brief_title("# TASK\nDo the thing. Then more.\n").as_deref(),
            Some("Do the thing.")
        );
    }

    #[test]
    fn extract_brief_title_no_heading_returns_none() {
        assert!(extract_brief_title("No heading here.\nDo things.\n").is_none());
    }

    #[test]
    fn extract_brief_title_heading_with_no_body_returns_none() {
        assert!(extract_brief_title("# Task\n\n## Next\nbody\n").is_none());
    }

    #[test]
    fn extract_brief_title_single_sentence_no_period() {
        let title = extract_brief_title("# Task\nA short description with no period\n");
        assert_eq!(title.as_deref(), Some("A short description with no period"));
    }

    #[test]
    fn extract_brief_title_version_number_not_sentence_boundary() {
        // "v1.0.0 is" — the period in v1.0.0 is not followed by a space, so it
        // is not a sentence boundary.
        let title = extract_brief_title("# Task\nShip v1.0.0 today. Then rest.\n");
        assert_eq!(title.as_deref(), Some("Ship v1.0.0 today."));
    }

    // --- load_task_titles (integration against a temp home) ---

    #[test]
    fn load_missing_home_is_empty() {
        let t = load_task_titles(Path::new("/nonexistent/fm-home-99999"));
        assert!(t.is_empty());
    }

    #[test]
    fn backlog_title_overrides_brief_title() {
        let tmp = tempfile_dir();
        let data = tmp.join("data");
        fs::create_dir_all(data.join("demo-task")).unwrap();
        fs::write(
            data.join("backlog.md"),
            "- [ ] demo-task - backlog version of the title (repo: x) (kind: y)\n",
        )
        .unwrap();
        fs::write(
            data.join("demo-task").join("brief.md"),
            "# Task\nBrief version. More.\n",
        )
        .unwrap();

        let titles = load_task_titles(&tmp);
        assert_eq!(
            titles.title_for("demo-task"),
            Some("backlog version of the title")
        );
    }

    #[test]
    fn brief_title_used_when_not_in_backlog() {
        let tmp = tempfile_dir();
        let data = tmp.join("data");
        fs::create_dir_all(data.join("brief-only")).unwrap();
        fs::write(
            data.join("brief-only").join("brief.md"),
            "# Task\nDo the thing.\n",
        )
        .unwrap();

        let titles = load_task_titles(&tmp);
        assert_eq!(titles.title_for("brief-only"), Some("Do the thing."));
    }

    /// Create a unique temp dir under /tmp for this test run. Cleaned up is
    /// best-effort (temp dir is removed when the OS reclaims /tmp); we only
    /// need isolation within one test process.
    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "crew-watch-titles-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
