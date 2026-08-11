//! Firstmate fleet-record parsing.
//!
//! Firstmate writes one `state/<task-id>.meta` file per task, containing
//! `key=value` lines (`worktree=`, `project=`, `endpoint_task_id=`,
//! `harness=`, `window=`, ...). crew-watch matches an agent process to a
//! record by its cwd (the worktree path).
//!
//! Everything here is read-only and best-effort: a missing/unparseable home or
//! record never propagates as an error.

use std::fs;
use std::path::Path;

/// Subset of a firstmate `*.meta` record. Only the keys crew-watch needs are
/// parsed; unknown keys are ignored.
#[derive(Debug, Clone, Default)]
pub struct TaskRecord {
    pub task_id: String,
    pub worktree: Option<String>,
    pub project: Option<String>,
    pub harness: Option<String>,
    pub window: Option<String>,
}

/// Parse one `*.meta` file's contents. `task_id` falls back to the empty
/// string; [`load_fm_home`] fills it from the filename if absent.
pub fn parse_meta(content: &str) -> TaskRecord {
    let mut r = TaskRecord::default();
    for line in content.lines() {
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim().to_string();
        match key {
            "endpoint_task_id" => r.task_id = val,
            "worktree" => r.worktree = Some(val),
            "project" => r.project = Some(val),
            "harness" => r.harness = Some(val),
            "window" => r.window = Some(val),
            _ => {}
        }
    }
    r
}

/// Load every `state/*.meta` record under a firstmate home. Never fails: a
/// missing dir or unreadable file is silently skipped.
pub fn load_fm_home(home: &Path) -> Vec<TaskRecord> {
    let state = home.join("state");
    let Ok(entries) = fs::read_dir(&state) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("meta") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let mut rec = parse_meta(&content);
        if rec.task_id.is_empty() {
            rec.task_id = stem;
        }
        out.push(rec);
    }
    out
}

/// Find the record whose `worktree` equals `cwd`. Paths are compared without a
/// trailing slash so a readlink `/proc/<pid>/cwd` (never trailing) matches.
pub fn find_by_cwd<'a>(records: &'a [TaskRecord], cwd: &Path) -> Option<&'a TaskRecord> {
    let target = cwd.to_string_lossy();
    let target = target.trim_end_matches('/');
    records.iter().find(|r| {
        r.worktree
            .as_deref()
            .map(|w| w.trim_end_matches('/') == target)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SAMPLE: &str = "window=firstmate:fm-crew-watch-v1\n\
endpoint_task_id=crew-watch-v1\n\
worktree=/home/crew/.treehouse/crew-watch-c429f6/1/crew-watch\n\
project=/home/crew/agents/firstmate/projects/crew-watch\n\
harness=opencode\n\
kind=ship\n\
mode=no-mistakes\n\
busy_gen=g1786446653.28322.9129\n";

    #[test]
    fn parse_known_keys_ignores_rest() {
        let r = parse_meta(SAMPLE);
        assert_eq!(r.task_id, "crew-watch-v1");
        assert_eq!(
            r.worktree.as_deref(),
            Some("/home/crew/.treehouse/crew-watch-c429f6/1/crew-watch")
        );
        assert_eq!(
            r.project.as_deref(),
            Some("/home/crew/agents/firstmate/projects/crew-watch")
        );
        assert_eq!(r.harness.as_deref(), Some("opencode"));
        assert_eq!(r.window.as_deref(), Some("firstmate:fm-crew-watch-v1"));
    }

    #[test]
    fn parse_empty_is_default() {
        let r = parse_meta("");
        assert!(r.task_id.is_empty());
        assert!(r.worktree.is_none());
    }

    #[test]
    fn parse_handles_blank_and_garbage_lines() {
        let r = parse_meta("\nno equals here\n=nonkey\nworktree=/x/y\n");
        assert_eq!(r.worktree.as_deref(), Some("/x/y"));
    }

    #[test]
    fn find_by_cwd_matches_with_or_without_trailing_slash() {
        let recs = vec![parse_meta(SAMPLE)];
        let cwd = PathBuf::from("/home/crew/.treehouse/crew-watch-c429f6/1/crew-watch/");
        assert!(find_by_cwd(&recs, &cwd).is_some());
        let cwd2 = PathBuf::from("/home/crew/.treehouse/crew-watch-c429f6/1/crew-watch");
        assert!(find_by_cwd(&recs, &cwd2).is_some());
    }

    #[test]
    fn find_by_cwd_miss() {
        let recs = vec![parse_meta(SAMPLE)];
        let cwd = PathBuf::from("/elsewhere");
        assert!(find_by_cwd(&recs, &cwd).is_none());
    }

    #[test]
    fn load_missing_home_is_empty() {
        let recs = load_fm_home(Path::new("/nonexistent/firstmate-home-12345"));
        assert!(recs.is_empty());
    }
}
