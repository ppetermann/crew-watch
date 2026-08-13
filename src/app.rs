//! Application state owned by the main loop.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::detect::{build_sessions, Session};
use crate::meta::{load_fm_home, TaskRecord};
use crate::model::resolve_model;
use crate::procfs::{collect, Snapshot};
use crate::quota::{has_usage_windows, QuotaFetch, QuotaReport};
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
    // --- quota row state (never touched by tick(); see src/quota.rs) ---
    /// False under `--no-quota`: the row is absent and nothing is fetched.
    pub quota_enabled: bool,
    /// Poller cadence (clamped 60..=3600s). Used for the staleness threshold.
    pub quota_interval: Duration,
    /// `None` ⇒ auto mode (providers with ≥1 window). `Some(ids)` ⇒ explicit.
    pub quota_selected: Option<Vec<String>>,
    /// Channel from the background poller; `None` when disabled / `--once`.
    pub quota_rx: Option<Receiver<QuotaFetch>>,
    pub quota: QuotaState,
    /// Transient one-line message shown in the help line until next keypress
    /// (e.g. a config-save failure). Cleared by any key in the main loop.
    pub notice: Option<String>,
    /// Provider-selection dialog; `Some` while open, rendered as an overlay.
    pub dialog: Option<crate::quota_dialog::QuotaDialog>,
}

/// Last-known quota fetch state. On a [`QuotaFetch::Failed`] the previous report
/// is retained (rendered dim) so a transient outage never blanks the row.
#[derive(Default)]
pub struct QuotaState {
    pub report: Option<QuotaReport>,
    pub last_ok_at: Option<Instant>,
    pub last_error: Option<String>,
}

/// The effective provider selection driving the row and `--once`.
#[derive(Debug, Clone)]
pub enum QuotaSelection {
    /// Show every provider that reports ≥1 window.
    Auto,
    /// Show exactly these provider ids (report order at render time).
    Explicit(Vec<String>),
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
            quota_enabled: true,
            quota_interval: Duration::from_secs(300),
            quota_selected: None,
            quota_rx: None,
            quota: QuotaState::default(),
            notice: None,
            dialog: None,
        }
    }

    /// Collect one /proc snapshot, rebuild sessions, and resolve task info.
    pub fn tick(&mut self) {
        // Reload BEFORE resolving: this tick's labels must come from this
        // tick's records, not from whatever occupied a (recycled) worktree at
        // launch. See the `src/taskinfo.rs` header for why that matters.
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

    // --- quota (none of this rides the /proc tick path) ---

    /// Drain all pending quota fetches from the background poller, non-blocking.
    /// A [`QuotaFetch::Report`] updates state and refreshes `last_ok_at`; a
    /// [`QuotaFetch::Failed`] records the error but keeps the last good report
    /// so the row dims rather than vanishing.
    pub fn drain_quota(&mut self) {
        let Some(rx) = &self.quota_rx else {
            return;
        };
        while let Ok(fetch) = rx.try_recv() {
            match fetch {
                QuotaFetch::Report(r) => {
                    self.quota.report = Some(r);
                    self.quota.last_ok_at = Some(Instant::now());
                    self.quota.last_error = None;
                }
                QuotaFetch::Failed(e) => {
                    self.quota.last_error = Some(e);
                }
            }
        }
    }

    /// The effective provider selection: explicit (from config) or auto.
    pub fn effective_selection(&self) -> QuotaSelection {
        match &self.quota_selected {
            Some(ids) => QuotaSelection::Explicit(ids.clone()),
            None => QuotaSelection::Auto,
        }
    }

    /// Number of quota lines to render this frame (the height of the quota slot).
    /// 0 when disabled, auto with no provider reporting windows, or an
    /// explicit-empty selection — in all those cases the slot vanishes and the
    /// layout is byte-identical to pre-quota crew-watch.
    pub fn quota_lines_count(&self) -> usize {
        if !self.quota_enabled {
            return 0;
        }
        match self.effective_selection() {
            QuotaSelection::Explicit(ids) => {
                if ids.is_empty() {
                    return 0; // explicit empty ⇒ row hidden
                }
                match &self.quota.report {
                    Some(r) => r.providers.iter().filter(|p| ids.contains(&p.id)).count(),
                    None => 1, // fetch-level unavailable line (D9: explicit ⇒ visible failure)
                }
            }
            QuotaSelection::Auto => self
                .quota
                .report
                .as_ref()
                .map(|r| r.providers.iter().filter(|p| has_usage_windows(p)).count())
                .unwrap_or(0),
        }
    }

    /// The fetch age, if the row should be rendered dim because the poller has
    /// missed roughly two consecutive intervals (`age > 3 × cadence`). The
    /// threshold is relative to cadence so a 5-minute cadence stays "fresh"
    /// looking for ~15 minutes — it never reads as perpetually stale.
    pub fn quota_fetch_age(&self) -> Option<Duration> {
        let ok_at = self.quota.last_ok_at?;
        let age = ok_at.elapsed();
        if age > self.quota_interval * 3 {
            Some(age)
        } else {
            None
        }
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

    // --- quota selection / line counting (D7, D9 legs) ---

    use crate::quota::{ProviderQuota, ProviderStatus, QuotaFetch, QuotaReport, QuotaWindow};

    fn quota_report_with(claude_live: bool) -> QuotaReport {
        let claude = if claude_live {
            ProviderQuota {
                id: "claude".to_string(),
                label: "Claude".to_string(),
                plan: Some("max".to_string()),
                windows: vec![QuotaWindow {
                    id: "five_hour".to_string(),
                    label: "session".to_string(),
                    percent_used: 5.0,
                    resets_at: None,
                }],
                status: ProviderStatus::Fresh,
                stale: false,
                error: None,
            }
        } else {
            ProviderQuota {
                id: "claude".to_string(),
                label: "Claude".to_string(),
                plan: None,
                windows: vec![],
                status: ProviderStatus::Error,
                stale: false,
                error: Some("x".to_string()),
            }
        };
        QuotaReport {
            schema_version: Some(3),
            generated_at: String::new(),
            providers: vec![claude],
        }
    }

    #[test]
    fn effective_selection_auto_vs_explicit() {
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        assert!(matches!(app.effective_selection(), QuotaSelection::Auto));
        app.quota_selected = Some(vec!["claude".to_string()]);
        assert!(matches!(
            app.effective_selection(),
            QuotaSelection::Explicit(_)
        ));
    }

    #[test]
    fn quota_lines_auto_live_then_failed() {
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        // auto, no report yet -> 0 lines (row absent until first fetch lands)
        assert_eq!(app.quota_lines_count(), 0);
        app.quota.report = Some(quota_report_with(true));
        assert_eq!(app.quota_lines_count(), 1, "live claude -> 1 line");
        // provider loses its windows: auto has nothing to render
        app.quota.report = Some(quota_report_with(false));
        assert_eq!(app.quota_lines_count(), 0);
    }

    #[test]
    fn quota_lines_auto_counts_stale_and_unknown_status() {
        // Staleness (cache fallback) and an unrecognised status must not blank the
        // row: as long as a provider reports windows, auto mode gives it a line.
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        let mut report = quota_report_with(true);
        report.providers[0].stale = true;
        report.providers[0].status = ProviderStatus::Unknown("stale".to_string());
        app.quota.report = Some(report);
        assert_eq!(app.quota_lines_count(), 1);
    }

    #[test]
    fn quota_lines_explicit_d9_legs() {
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        // explicit empty -> row hidden
        app.quota_selected = Some(vec![]);
        assert_eq!(app.quota_lines_count(), 0);
        // explicit selection, no report -> one unavailable line (D9)
        app.quota_selected = Some(vec!["claude".to_string()]);
        assert_eq!(app.quota_lines_count(), 1);
        // explicit selection, report present -> the matching provider line
        app.quota.report = Some(quota_report_with(true));
        assert_eq!(app.quota_lines_count(), 1);
        // explicit selection of a vanished provider (not in report) -> omitted
        app.quota_selected = Some(vec!["zai".to_string()]);
        assert_eq!(app.quota_lines_count(), 0);
    }

    #[test]
    fn quota_lines_disabled_is_zero() {
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        app.quota_enabled = false;
        app.quota.report = Some(quota_report_with(true));
        app.quota_selected = Some(vec!["claude".to_string()]);
        assert_eq!(app.quota_lines_count(), 0);
    }

    #[test]
    fn drain_quota_keeps_last_report_on_failure() {
        use std::sync::mpsc::channel;
        let (tx, rx) = channel::<QuotaFetch>();
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        app.quota_rx = Some(rx);

        tx.send(QuotaFetch::Report(quota_report_with(true)))
            .unwrap();
        app.drain_quota();
        assert!(app.quota.report.is_some());
        assert!(app.quota.last_ok_at.is_some());

        // A subsequent failure must NOT wipe the last good report.
        tx.send(QuotaFetch::Failed("timed out".to_string()))
            .unwrap();
        app.drain_quota();
        assert!(
            app.quota.report.is_some(),
            "last report retained on failure"
        );
        assert_eq!(app.quota.last_error.as_deref(), Some("timed out"));
    }

    #[test]
    fn drain_quota_without_receiver_is_noop() {
        // --once path: no poller, no receiver. Must not panic.
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        app.drain_quota();
        assert!(app.quota.report.is_none());
    }
}
