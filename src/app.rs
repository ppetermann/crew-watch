//! Application state owned by the main loop.

use std::path::PathBuf;
use std::time::Duration;

use crate::detect::{build_sessions, Session};
use crate::meta::{load_fm_home, TaskRecord};
use crate::model::resolve_model;
use crate::procfs::{collect, Snapshot};
use crate::taskinfo::TaskInfo;
use crate::titles::{load_task_titles, TaskTitles};

pub struct App {
    pub interval: Duration,
    /// Firstmate home. Fleet records and task titles are re-read from here on
    /// every tick so a long-running instance tracks task lifecycle instead of
    /// freezing whatever was on disk at launch. See the `src/taskinfo.rs`
    /// header for why this freshness matters.
    pub fm_home: PathBuf,
    pub records: Vec<TaskRecord>,
    pub titles: TaskTitles,
    pub curr: Option<Snapshot>,
    pub prev: Option<Snapshot>,
    pub sessions: Vec<Session>,
}

impl App {
    pub fn new(interval: Duration, fm_home: PathBuf) -> Self {
        let records = load_fm_home(&fm_home);
        let titles = load_task_titles(&fm_home);
        Self {
            interval,
            fm_home,
            records,
            titles,
            curr: None,
            prev: None,
            sessions: Vec::new(),
        }
    }

    /// Collect one /proc snapshot, rebuild sessions, and resolve task info.
    pub fn tick(&mut self) {
        // Re-read fleet records and titles BEFORE resolving, so a task spawned
        // after launch — or a worker copy recycled from a finished task to a
        // new one in the same reused worktree — is labelled with its current
        // title, not whatever occupied that path when crew-watch started. Both
        // loaders are best-effort and never fail, so a fleet home that vanishes
        // mid-run yields empty records (agents then fall through to the
        // project / argv layers) instead of crashing or keeping ghost labels.
        self.reload_fleet();

        let snap = collect();
        let mut sessions = build_sessions(&snap, self.curr.as_ref());
        let records = &self.records;
        let titles = &self.titles;
        for s in &mut sessions {
            if let Some(entry) = snap.procs.get(&s.pid) {
                s.task = TaskInfo::resolve(
                    records,
                    titles,
                    entry.cwd.as_deref(),
                    || {
                        entry
                            .cwd
                            .as_deref()
                            .and_then(crate::project::resolve_project_name)
                    },
                    &entry.cmdline,
                    s.kind,
                );
                s.model = resolve_model(&entry.cmdline).unwrap_or_default();
            }
        }
        self.prev = self.curr.take();
        self.curr = Some(snap);
        self.sessions = sessions;
    }

    /// Re-read firstmate fleet records and task titles from `fm_home`.
    /// Best-effort: a missing or unreadable source yields empty data, so the
    /// resolution layering falls back to the project-name / argv layers rather
    /// than retaining stale labels or propagating an error.
    fn reload_fleet(&mut self) {
        self.records = load_fm_home(&self.fm_home);
        self.titles = load_task_titles(&self.fm_home);
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.curr.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AGENT_KINDS;
    use crate::meta::find_by_cwd;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn kind(id: &str) -> &'static crate::detect::AgentKind {
        AGENT_KINDS.iter().find(|k| k.id == id).unwrap()
    }

    /// Create a unique temp dir under /tmp for this test run. Cleaned up is
    /// best-effort (the OS reclaims /tmp); we only need isolation within one
    /// test process. Mirrors the helper in `src/titles.rs`.
    fn tempfile_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "crew-watch-app-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a `state/<task-id>.meta` record and return nothing.
    fn write_meta(home: &Path, task_id: &str, worktree: &str) {
        let state = home.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join(format!("{}.meta", task_id)),
            format!(
                "endpoint_task_id={}\nworktree={}\nharness=opencode\n",
                task_id, worktree
            ),
        )
        .unwrap();
    }

    fn write_backlog(home: &Path, content: &str) {
        let data = home.join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("backlog.md"), content).unwrap();
    }

    const WORKTREE: &str = "/home/crew/.treehouse/firstmate-89ec77/1/firstmate";

    // --- reload_fleet: the actual defect ---

    #[test]
    fn reload_picks_up_recycled_worktree_new_task() {
        // The observed bug: crew-watch loaded fleet state once at launch, then
        // a worker copy was recycled from a finished task to a new one in the
        // SAME reused worktree path. The stale snapshot still attributed that
        // path to the finished task, so the new agent was mislabelled. Here a
        // fresh App is built against task-A, the on-disk record is then swapped
        // to task-B at the identical worktree, and reload_fleet must make
        // resolution converge on task-B's title.
        let home = tempfile_dir("recycled");
        write_meta(&home, "finished-task", WORKTREE);
        write_backlog(
            &home,
            "- [x] finished-task - give each worker its own browser session (repo: x) (done 2026-08-12)\n\
             - [ ] fresh-scout - scout the new fleet layout (repo: x) (kind: ship)\n",
        );

        let mut app = App::new(Duration::from_secs(2), home.clone());
        let cwd = PathBuf::from(WORKTREE);
        let cmdline = vec!["opencode".to_string()];

        // At construction the worktree is still occupied by finished-task.
        let initial = TaskInfo::resolve(
            &app.records,
            &app.titles,
            Some(&cwd),
            || None,
            &cmdline,
            kind("opencode"),
        );
        assert_eq!(
            initial,
            "give each worker its own browser session [finished-task]"
        );

        // Firstmate recycles the worktree: finished-task's meta is removed and
        // fresh-scout takes over the SAME path.
        fs::remove_file(home.join("state").join("finished-task.meta")).unwrap();
        write_meta(&home, "fresh-scout", WORKTREE);

        // reload_fleet is exactly what tick() now calls before resolving.
        app.reload_fleet();

        let after = TaskInfo::resolve(
            &app.records,
            &app.titles,
            Some(&cwd),
            || None,
            &cmdline,
            kind("opencode"),
        );
        assert_eq!(
            after, "scout the new fleet layout [fresh-scout]",
            "recycled worktree must show the new task, not the finished one"
        );
    }

    #[test]
    fn reload_drops_vanished_record_no_ghost() {
        // Acceptance: an agent whose record genuinely disappears (task torn
        // down, no replacement) must stop claiming the old task rather than
        // keeping a ghost label. After reload the record is gone, so layer 1
        // no longer matches and resolution falls through.
        let home = tempfile_dir("vanished");
        write_meta(&home, "doomed-task", WORKTREE);
        write_backlog(
            &home,
            "- [ ] doomed-task - soon to be torn down (repo: x)\n",
        );

        let mut app = App::new(Duration::from_secs(2), home.clone());
        let cwd = PathBuf::from(WORKTREE);
        assert!(find_by_cwd(&app.records, &cwd).is_some());

        // Task is torn down; no replacement reuses the worktree.
        fs::remove_file(home.join("state").join("doomed-task.meta")).unwrap();
        app.reload_fleet();

        assert!(
            find_by_cwd(&app.records, &cwd).is_none(),
            "no ghost record after teardown"
        );
        // And resolution no longer claims the dead task id.
        let line = TaskInfo::resolve(
            &app.records,
            &app.titles,
            Some(&cwd),
            || Some("firstmate".to_string()),
            &["opencode".to_string()],
            kind("opencode"),
        );
        assert!(
            !line.contains("doomed-task"),
            "fallen-through line must not carry the dead task id, got: {line}"
        );
    }

    // --- non-fatal sources ---

    #[test]
    fn construct_with_missing_fm_home_is_non_fatal() {
        // crew-watch may be pointed at a home that does not exist yet.
        // Construction must succeed and yield empty records/titles.
        let app = App::new(
            Duration::from_secs(2),
            PathBuf::from("/nonexistent/fm-home-app-test-99999"),
        );
        assert!(app.records.is_empty());
    }

    #[test]
    fn reload_survives_vanished_fm_home() {
        // A fleet home that vanishes mid-run must never crash the monitor.
        // reload_fleet re-reads best-effort and yields empty data.
        let home = tempfile_dir("vanish-midrun");
        write_meta(&home, "some-task", WORKTREE);

        let mut app = App::new(Duration::from_secs(2), home.clone());
        assert_eq!(app.records.len(), 1);

        // Wipe the whole home out from under the running monitor.
        fs::remove_dir_all(&home).unwrap();
        app.reload_fleet();

        assert!(
            app.records.is_empty(),
            "vanished home yields empty, no panic"
        );
    }
}
