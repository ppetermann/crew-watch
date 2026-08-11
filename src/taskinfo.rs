//! Layered "what is it working on" resolution.
//!
//! Sources are consulted in order; the first one to return a non-empty answer
//! wins. Today there are two layers:
//! 1. firstmate fleet records ([`crate::meta`]), matched by the agent's cwd;
//! 2. a process-derived fallback: cwd basename plus a trimmed argv excerpt.
//!
//! The layering mirrors the detection table: a new fleet format is a new
//! `TaskInfoSource` implementation prepended here.

use std::path::Path;

use crate::detect::AgentKind;
use crate::meta::{find_by_cwd, TaskRecord};

/// A one-line description of an agent's current work.
pub struct TaskInfo;

const ARGV_EXCERPT_MAX_TOKENS: usize = 2;
const ARGV_EXCERPT_MAX_CHARS: usize = 40;
const ARGV_TOKEN_MAX_CHARS: usize = 60;

impl TaskInfo {
    /// Resolve the task line for an agent process. Pure over its inputs so it
    /// can be unit-tested with fixture data.
    pub fn resolve(
        records: &[TaskRecord],
        cwd: Option<&Path>,
        cmdline: &[String],
        kind: &AgentKind,
    ) -> String {
        // Layer 1: firstmate records, matched by worktree == cwd.
        if let Some(cwd) = cwd {
            if let Some(rec) = find_by_cwd(records, cwd) {
                let project = rec
                    .project
                    .as_deref()
                    .and_then(|p| Path::new(p).file_name())
                    .map(|s| s.to_string_lossy().into_owned());
                return match project {
                    Some(p) => format!("{} ({})", rec.task_id, p),
                    None => rec.task_id.clone(),
                };
            }
        }

        // Layer 2: cwd basename plus a trimmed argv excerpt.
        let cwd_base = cwd
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned());
        let excerpt = argv_excerpt(cmdline);
        match (cwd_base, excerpt) {
            (Some(c), Some(e)) => format!("{}: {}", c, e),
            (Some(c), None) => c,
            (None, Some(e)) => e,
            (None, None) => kind.display.to_string(),
        }
    }
}

/// Build a short argv excerpt from argv[1..], skipping very long tokens (e.g.
/// a multi-KiB embedded prompt) and capping overall length.
fn argv_excerpt(cmdline: &[String]) -> Option<String> {
    let mut out = String::new();
    let mut tokens = 0;
    for tok in cmdline.iter().skip(1) {
        if tok.len() > ARGV_TOKEN_MAX_CHARS || tok.starts_with("--prompt") {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(tok);
        tokens += 1;
        if tokens >= ARGV_EXCERPT_MAX_TOKENS || out.len() >= ARGV_EXCERPT_MAX_CHARS {
            break;
        }
    }
    if out.len() > ARGV_EXCERPT_MAX_CHARS {
        out.truncate(ARGV_EXCERPT_MAX_CHARS - 3);
        out.push_str("...");
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AGENT_KINDS;
    use crate::meta::parse_meta;
    use std::path::PathBuf;

    fn kind(id: &str) -> &'static AgentKind {
        AGENT_KINDS.iter().find(|k| k.id == id).unwrap()
    }

    const SAMPLE_META: &str = "endpoint_task_id=crew-watch-v1\nworktree=/home/crew/wt/crew-watch\n\
project=/home/crew/agents/firstmate/projects/crew-watch\nharness=opencode\n";

    #[test]
    fn first_layer_meta_wins() {
        let recs = vec![parse_meta(SAMPLE_META)];
        let cwd = PathBuf::from("/home/crew/wt/crew-watch");
        let cmdline = vec![
            "opencode".to_string(),
            "--model".to_string(),
            "glm-5.2".to_string(),
        ];
        let line = TaskInfo::resolve(&recs, Some(&cwd), &cmdline, kind("opencode"));
        assert_eq!(line, "crew-watch-v1 (crew-watch)");
    }

    #[test]
    fn fallback_cwd_plus_argv_excerpt() {
        let cwd = PathBuf::from("/home/crew/wt/crew-watch");
        let cmdline = vec![
            "opencode".to_string(),
            "--model".to_string(),
            "glm-5.2".to_string(),
        ];
        let line = TaskInfo::resolve(&[], Some(&cwd), &cmdline, kind("opencode"));
        assert_eq!(line, "crew-watch: --model glm-5.2");
    }

    #[test]
    fn fallback_skips_huge_prompt_token() {
        let cwd = PathBuf::from("/wt/proj");
        let huge = "x".repeat(10_000);
        let cmdline = vec![
            "claude".to_string(),
            "--prompt".to_string(),
            huge,
            "--model".to_string(),
            "opus".to_string(),
        ];
        let line = TaskInfo::resolve(&[], Some(&cwd), &cmdline, kind("claude"));
        // The giant prompt is skipped; --prompt key is also skipped by name.
        assert_eq!(line, "proj: --model opus");
    }

    #[test]
    fn fallback_no_cwd_uses_kind() {
        let line = TaskInfo::resolve(&[], None, &[], kind("claude"));
        assert_eq!(line, "claude");
    }

    #[test]
    fn fallback_truncates_long_excerpt() {
        let cwd = PathBuf::from("/wt/x");
        let long = "a".repeat(200);
        let cmdline = vec!["opencode".to_string(), long];
        let line = TaskInfo::resolve(&[], Some(&cwd), &cmdline, kind("opencode"));
        // Token longer than 60 chars is skipped entirely -> falls to cwd basename.
        assert_eq!(line, "x");
    }
}
