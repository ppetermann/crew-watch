//! Layered "what is it working on" resolution.
//!
//! Sources are consulted in order; the first to answer wins:
//! 1. **Firstmate fleet records** ([`crate::meta`]), matched by the agent's cwd.
//!    When the task id resolves to a human title from [`crate::titles`], the
//!    column reads like a sentence: `fix away-mode resurface flood
//!    [fm-afk-resurface-loop]`. The title is shown first, the task id in
//!    brackets as a secondary reference.
//! 2. **Unmatched with a cwd**: the cwd basename as the project name, labelled
//!    `interactive @ <project>` when no prompt arg is detected (an interactive
//!    session the captain is driving). Bare flag noise like `-p --verbose` is
//!    never shown alone.
//! 3. **No cwd at all**: a trimmed positional argv excerpt, else the runtime
//!    name.
//!
//! The layering mirrors the detection table: to support a new fleet format,
//! prepend another lookup step at the top of [`TaskInfo::resolve`].

use std::path::Path;

use crate::detect::AgentKind;
use crate::meta::{find_by_cwd, TaskRecord};
use crate::titles::TaskTitles;

/// A one-line description of an agent's current work.
pub struct TaskInfo;

/// Title is capped so the TASK column stays readable even when the backlog
/// entry is a multi-sentence paragraph. The terminal renders further truncation
/// to the actual column width.
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
    /// `project_name` is an optional pre-computed project name (the caller may
    /// resolve it from git); when `None`, the cwd basename is used.
    pub fn resolve(
        records: &[TaskRecord],
        titles: &TaskTitles,
        cwd: Option<&Path>,
        project_name: Option<&str>,
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
        let project = project_name.map(|s| s.to_string()).or_else(|| {
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

/// Detect whether argv carries a prompt/task argument (a sign the session is
/// autonomous rather than interactive). True when:
/// - a `--prompt` flag is present (with or without a following value), or
/// - a positional token (not starting with `-`) is longer than the threshold.
fn has_prompt_arg(cmdline: &[String]) -> bool {
    for tok in cmdline.iter().skip(1) {
        if tok == "--prompt" {
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
        let line = TaskInfo::resolve(&recs, &titles, Some(&cwd), None, &cmdline, kind("opencode"));
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
        let line = TaskInfo::resolve(&recs, &titles, Some(&cwd), None, &[], kind("opencode"));
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
        let line = TaskInfo::resolve(&recs, &titles, Some(&cwd), None, &[], kind("opencode"));
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
            None,
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
            None,
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
            Some("firstmate"),
            &["claude".to_string()],
            kind("claude"),
        );
        assert_eq!(line, "interactive @ firstmate");
    }

    #[test]
    fn unmatched_never_shows_bare_flags() {
        // The captain's observed complaint: `-p --verbose` alone. After the fix
        // only the project name is shown.
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
            None,
            &cmdline,
            kind("opencode"),
        );
        assert_eq!(line, "interactive @ crew-watch");
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
            &cmdline,
            kind("claude"),
        );
        assert_eq!(line, "claude");
    }

    #[test]
    fn no_cwd_empty_cmdline_uses_kind() {
        let line = TaskInfo::resolve(&[], &TaskTitles::default(), None, None, &[], kind("claude"));
        assert_eq!(line, "claude");
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
