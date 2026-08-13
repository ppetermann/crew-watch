//! Layered "what is it working on" resolution.
//!
//! Sources are consulted in order; the first to answer wins:
//! 1. **Firstmate fleet records** ([`crate::meta`]), matched by the agent's cwd.
//!    When the task id resolves to a human title from [`crate::titles`], the
//!    column reads like a sentence: `fix away-mode resurface flood
//!    [fm-afk-resurface-loop]`. The title is shown first, the task id in
//!    brackets as a secondary reference.
//! 2. **Unmatched with a cwd**: the project name (git repo name via
//!    [`crate::project`], else the cwd basename), labelled
//!    `interactive @ <project>` when no prompt arg is detected (an interactive
//!    session the captain is driving). Bare flag noise like `-p --verbose` is
//!    never shown alone.
//! 3. **No cwd at all**: a trimmed positional argv excerpt, else the runtime
//!    name.
//!
//! The layering mirrors the detection table: to support a new fleet format,
//! prepend another lookup step at the top of [`TaskInfo::resolve`].
//!
//! ### Record freshness (why the caller reloads per tick)
//!
//! Layer 1 matches an agent to a task by the process's cwd, and firstmate
//! hands out worker copies from a **recycled worktree pool**: the same path is
//! reused by task after task. So the fleet records and titles passed in here
//! must reflect *current* on-disk state, not whatever was read when crew-watch
//! launched — otherwise an agent spawned after launch inherits a path that, in
//! a stale snapshot, still belongs to a task that finished hours ago, and gets
//! rendered with that dead task's title at full confidence. [`crate::app::App`]
//! therefore re-reads both sources from the firstmate home on every tick;
//! [`crate::meta::load_fm_home`] and [`crate::titles::load_task_titles`] are
//! best-effort, so a vanished source degrades to the lower layers instead of
//! keeping a ghost label. The resolution logic itself is correct as written —
//! only the age of its inputs was the bug.

use std::path::Path;

use crate::detect::AgentKind;
use crate::meta::{find_by_cwd, TaskRecord};
use crate::titles::TaskTitles;

/// A one-line description of an agent's current work.
pub struct TaskInfo;

/// Title is capped so the TASK column stays readable even when the backlog
/// entry is a multi-sentence paragraph. Width-aware fitting to the actual
/// column happens at render time via [`fit_task_line`].
const TITLE_DISPLAY_MAX: usize = 80;
/// A positional argv token longer than this is treated as a prompt blob (not
/// shown, and its presence marks the session as non-interactive).
const PROMPT_LEN_THRESHOLD: usize = 100;
/// Max chars for the positional argv excerpt when no cwd is available.
const ARGV_EXCERPT_MAX_CHARS: usize = 40;

impl TaskInfo {
    /// Resolve the task line for an agent process. Pure over its inputs so it
    /// can be unit-tested with fixture data.
    ///
    /// `project_name` lazily supplies a pre-computed project name (the caller
    /// may resolve it from git); it is only invoked for sessions that are not
    /// fleet-matched, and when it yields `None` the cwd basename is used.
    pub fn resolve(
        records: &[TaskRecord],
        titles: &TaskTitles,
        cwd: Option<&Path>,
        project_name: impl FnOnce() -> Option<String>,
        cmdline: &[String],
        kind: &AgentKind,
    ) -> String {
        // Layer 1: firstmate fleet records, matched by worktree == cwd.
        if let Some(cwd) = cwd {
            if let Some(rec) = find_by_cwd(records, cwd) {
                return fleet_task_line(rec, titles);
            }
        }

        // Layer 2: unmatched — prefer the project name over argv.
        let project = project_name().or_else(|| {
            cwd.and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
        });
        if let Some(project) = project {
            if has_prompt_arg(cmdline) {
                // Autonomous session whose task we could not identify: show
                // where it is working without the misleading "interactive".
                return project;
            }
            return format!("interactive @ {}", project);
        }

        // Layer 3: no cwd — positional argv excerpt, else the runtime name.
        if let Some(excerpt) = positional_argv_excerpt(cmdline) {
            return excerpt;
        }
        kind.display.to_string()
    }
}

/// Build the task line for a fleet-matched agent: `<title> [<task_id>]` when a
/// human title is known, else just the task id.
fn fleet_task_line(rec: &TaskRecord, titles: &TaskTitles) -> String {
    match titles.title_for(&rec.task_id) {
        Some(title) => {
            let title = truncate_with_ellipsis(title.trim(), TITLE_DISPLAY_MAX);
            format!("{} [{}]", title, rec.task_id)
        }
        None => rec.task_id.clone(),
    }
}

/// Fit a task line into `width` display columns. The full `title [task-id]`
/// line is kept when it fits; when space is tight the bracketed id is dropped
/// first, and only then is the remaining title ellipsized to the width.
pub fn fit_task_line(line: &str, width: usize) -> String {
    if line.chars().count() <= width {
        return line.to_string();
    }
    if let Some(title) = strip_id_suffix(line) {
        if title.chars().count() <= width {
            return title.to_string();
        }
        return truncate_with_ellipsis(title, width);
    }
    truncate_with_ellipsis(line, width)
}

/// Strip a trailing ` [task-id]` suffix from a task line, returning the title
/// part. Returns `None` when the line has no such suffix.
fn strip_id_suffix(line: &str) -> Option<&str> {
    if !line.ends_with(']') {
        return None;
    }
    let idx = line.rfind(" [")?;
    Some(line[..idx].trim_end())
}

/// Truncate a string to `max_chars` Unicode scalar values, appending `...` if
/// truncation occurred. Truncation always lands on a UTF-8 char boundary.
pub fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    if max_chars <= 3 {
        return s.chars().take(max_chars).collect();
    }
    let cut = max_chars - 3;
    let mut out: String = s.chars().take(cut).collect();
    out.push_str("...");
    out
}

/// Detect whether argv marks the session as autonomous rather than
/// interactive. True when:
/// - a `--prompt` flag is present (with or without a following value), or
/// - a headless/print-mode flag (`-p`, `--print`, `--headless`) is present, or
/// - a positional token (not starting with `-`) is longer than the threshold.
fn has_prompt_arg(cmdline: &[String]) -> bool {
    for tok in cmdline.iter().skip(1) {
        if tok == "--prompt" || tok == "-p" || tok == "--print" || tok == "--headless" {
            return true;
        }
        if tok.starts_with("--prompt=") {
            return true;
        }
        if !tok.starts_with('-') && tok.len() > PROMPT_LEN_THRESHOLD {
            return true;
        }
    }
    false
}

/// Extract a short excerpt of meaningful positional arguments from argv[1..]:
/// flags, their values, and prompt blobs are skipped, so the result never
/// contains opaque flag noise. Returns `None` when nothing meaningful remains.
fn positional_argv_excerpt(cmdline: &[String]) -> Option<String> {
    /// Flags that consume the following token as their value (separate form).
    const VALUE_FLAGS: &[&str] = &["--model", "--effort", "--prompt"];

    let mut out = String::new();
    let mut skip_next = false;
    for tok in cmdline.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if tok.starts_with('-') {
            if VALUE_FLAGS.contains(&tok.as_str()) {
                skip_next = true;
            }
            continue;
        }
        if tok.len() > PROMPT_LEN_THRESHOLD {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(tok);
        if out.len() >= ARGV_EXCERPT_MAX_CHARS {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(truncate_with_ellipsis(&out, ARGV_EXCERPT_MAX_CHARS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AGENT_KINDS;
    use crate::meta::parse_meta;
    use crate::titles::parse_backlog;
    use std::path::PathBuf;

    fn kind(id: &str) -> &'static AgentKind {
        AGENT_KINDS.iter().find(|k| k.id == id).unwrap()
    }

    fn titles_from_backlog(content: &str) -> TaskTitles {
        TaskTitles::from_pairs(parse_backlog(content))
    }

    const SAMPLE_META: &str = "endpoint_task_id=fm-afk-resurface-loop\n\
worktree=/home/crew/wt/firstmate\n\
project=/home/crew/agents/firstmate/projects/firstmate\nharness=opencode\n";

    const SAMPLE_BACKLOG: &str = "\
- [ ] fm-afk-resurface-loop - away mode unusable: resurface fires ~1/sec (repo: firstmate) (kind: ship)
- [ ] crew-watch-v1 - build crew-watch v1 (repo: crew-watch) (kind: ship)
";

    // --- Layer 1: fleet-matched ---

    #[test]
    fn fleet_matched_shows_title_with_id() {
        let recs = vec![parse_meta(SAMPLE_META)];
        let titles = titles_from_backlog(SAMPLE_BACKLOG);
        let cwd = PathBuf::from("/home/crew/wt/firstmate");
        let cmdline = vec![
            "opencode".to_string(),
            "--model".to_string(),
            "zai-coding-plan/glm-5.2".to_string(),
        ];
        let line = TaskInfo::resolve(
            &recs,
            &titles,
            Some(&cwd),
            || None,
            &cmdline,
            kind("opencode"),
        );
        assert_eq!(
            line,
            "away mode unusable: resurface fires ~1/sec [fm-afk-resurface-loop]"
        );
    }

    #[test]
    fn fleet_matched_no_title_falls_back_to_id() {
        let recs = vec![parse_meta(SAMPLE_META)];
        let titles = TaskTitles::default();
        let cwd = PathBuf::from("/home/crew/wt/firstmate");
        let line = TaskInfo::resolve(&recs, &titles, Some(&cwd), || None, &[], kind("opencode"));
        assert_eq!(line, "fm-afk-resurface-loop");
    }

    #[test]
    fn fleet_matched_long_title_is_truncated() {
        let recs = vec![parse_meta(SAMPLE_META)];
        let long_title = format!(
            "- [ ] fm-afk-resurface-loop - {} (repo: firstmate) (kind: ship)",
            "a".repeat(200)
        );
        let titles = titles_from_backlog(&long_title);
        let cwd = PathBuf::from("/home/crew/wt/firstmate");
        let line = TaskInfo::resolve(&recs, &titles, Some(&cwd), || None, &[], kind("opencode"));
        // Title capped to 80 chars (77 + "..."), then id in brackets.
        let title_part = line.split(" [").next().unwrap();
        assert_eq!(title_part.chars().count(), 80);
        assert!(title_part.ends_with("..."));
        assert!(line.ends_with("[fm-afk-resurface-loop]"));
    }

    // --- Layer 2: unmatched with cwd ---

    #[test]
    fn unmatched_interactive_shows_project() {
        let cwd = PathBuf::from("/home/crew/firstmate");
        let cmdline = vec![
            "claude".to_string(),
            "--model".to_string(),
            "opus".to_string(),
        ];
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            Some(&cwd),
            || None,
            &cmdline,
            kind("claude"),
        );
        assert_eq!(line, "interactive @ firstmate");
    }

    #[test]
    fn unmatched_no_args_shows_interactive() {
        let cwd = PathBuf::from("/home/crew/firstmate");
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            Some(&cwd),
            || None,
            &["claude".to_string()],
            kind("claude"),
        );
        assert_eq!(line, "interactive @ firstmate");
    }

    #[test]
    fn unmatched_uses_project_name_override() {
        // A no-mistakes worktree: cwd basename is a ULID, but the caller
        // resolved the real repo name from git.
        let cwd = PathBuf::from("/home/crew/.nm/wt/abc/01KZRC1AP2FQZESTNY9C9HXA7A");
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            Some(&cwd),
            || Some("firstmate".to_string()),
            &["claude".to_string()],
            kind("claude"),
        );
        assert_eq!(line, "interactive @ firstmate");
    }

    #[test]
    fn unmatched_never_shows_bare_flags() {
        // The captain's observed complaint: `-p --verbose` alone. Only the
        // project name is shown, and `-p` marks the session headless, so no
        // "interactive" prefix either.
        let cwd = PathBuf::from("/wt/crew-watch");
        let cmdline = vec![
            "opencode".to_string(),
            "-p".to_string(),
            "--verbose".to_string(),
        ];
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            Some(&cwd),
            || None,
            &cmdline,
            kind("opencode"),
        );
        assert_eq!(line, "crew-watch");
    }

    #[test]
    fn unmatched_headless_flags_show_project_only() {
        // `--print` / `--headless` also mark the session autonomous even when
        // the prompt itself arrives via stdin.
        let cwd = PathBuf::from("/wt/crew-watch");
        for flag in ["--print", "--headless"] {
            let cmdline = vec!["claude".to_string(), flag.to_string()];
            let line = TaskInfo::resolve(
                &[],
                &TaskTitles::default(),
                Some(&cwd),
                || None,
                &cmdline,
                kind("claude"),
            );
            assert_eq!(line, "crew-watch");
        }
    }

    #[test]
    fn unmatched_with_prompt_arg_shows_project_only() {
        // A firstmate-launched task whose meta file we could not read: argv has
        // a prompt blob. Don't say "interactive" (it is autonomous), but show
        // where it is working.
        let cwd = PathBuf::from("/home/crew/.treehouse/firstmate-x/1/firstmate");
        let huge = "x".repeat(500);
        let cmdline = vec![
            "opencode".to_string(),
            "--model".to_string(),
            "glm-5.2".to_string(),
            "--prompt".to_string(),
            huge,
        ];
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            Some(&cwd),
            || None,
            &cmdline,
            kind("opencode"),
        );
        assert_eq!(line, "firstmate");
    }

    #[test]
    fn unmatched_with_positional_prompt_shows_project_only() {
        // claude-style: prompt is a trailing positional arg (no --prompt flag).
        let cwd = PathBuf::from("/wt/proj");
        let cmdline = vec![
            "claude".to_string(),
            "--model".to_string(),
            "opus".to_string(),
            "x".repeat(500),
        ];
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            Some(&cwd),
            || None,
            &cmdline,
            kind("claude"),
        );
        assert_eq!(line, "proj");
    }

    // --- Layer 3: no cwd ---

    #[test]
    fn no_cwd_shows_positional_argv_excerpt() {
        let cmdline = vec!["claude".to_string(), "fix the bug".to_string()];
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            None,
            || None,
            &cmdline,
            kind("claude"),
        );
        assert_eq!(line, "fix the bug");
    }

    #[test]
    fn no_cwd_skips_flags_and_prompt_blobs() {
        let cmdline = vec![
            "opencode".to_string(),
            "--model".to_string(),
            "glm-5.2".to_string(),
            "-p".to_string(),
            "--verbose".to_string(),
            "x".repeat(500),
            "do thing".to_string(),
        ];
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            None,
            || None,
            &cmdline,
            kind("opencode"),
        );
        // Only positional, non-huge tokens survive: "do thing".
        assert_eq!(line, "do thing");
    }

    #[test]
    fn no_cwd_no_meaningful_args_uses_kind() {
        let cmdline = vec![
            "claude".to_string(),
            "--model".to_string(),
            "opus".to_string(),
        ];
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            None,
            || None,
            &cmdline,
            kind("claude"),
        );
        assert_eq!(line, "claude");
    }

    #[test]
    fn no_cwd_empty_cmdline_uses_kind() {
        let line = TaskInfo::resolve(
            &[],
            &TaskTitles::default(),
            None,
            || None,
            &[],
            kind("claude"),
        );
        assert_eq!(line, "claude");
    }

    // --- fit_task_line ---

    #[test]
    fn fit_full_line_kept_when_it_fits() {
        let line = "fix flood [fm-afk-resurface-loop]";
        assert_eq!(fit_task_line(line, 40), line);
        assert_eq!(fit_task_line(line, line.chars().count()), line);
    }

    #[test]
    fn fit_drops_id_first_when_tight() {
        let line = "fix away-mode resurface flood [fm-afk-resurface-loop]";
        assert_eq!(fit_task_line(line, 30), "fix away-mode resurface flood");
    }

    #[test]
    fn fit_ellipsizes_title_when_even_title_is_too_wide() {
        let line = "fix away-mode resurface flood [fm-afk-resurface-loop]";
        assert_eq!(fit_task_line(line, 10), "fix awa...");
    }

    #[test]
    fn fit_line_without_id_suffix_gets_ellipsis() {
        assert_eq!(fit_task_line("interactive @ firstmate", 12), "interacti...");
    }

    #[test]
    fn fit_multibyte_title_is_char_boundary_safe() {
        let line = format!("{} [task-x]", "ä".repeat(30));
        let fitted = fit_task_line(&line, 10);
        assert_eq!(fitted.chars().count(), 10);
        assert!(fitted.ends_with("..."));
    }

    // --- truncate_with_ellipsis ---

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        assert_eq!(truncate_with_ellipsis("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_exact_boundary_no_ellipsis() {
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_multibyte_safe() {
        let s = "ä".repeat(25);
        let t = truncate_with_ellipsis(&s, 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with("..."));
    }

    #[test]
    fn truncate_tiny_max() {
        assert_eq!(truncate_with_ellipsis("hello", 3), "hel");
        assert_eq!(truncate_with_ellipsis("hello", 0), "");
    }
}
