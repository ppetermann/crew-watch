//! One refresh of everything crew-watch shows, without the terminal.
//!
//! [`Sampler`] is the TUI's tick pipeline as a library: per call it re-reads
//! the firstmate home's fleet records and task titles, takes one process
//! snapshot, rebuilds the agent sessions against the previous snapshot, and
//! resolves each session's task, model, activity and PR poll. The TUI keeps a
//! `Sampler` and renders its [`Session`]s; other programs call
//! [`Sampler::sample`] and get a serializable [`FleetSample`].
//!
//! The firstmate home is always the caller's choice: the library has no
//! default location (the TUI's `~/agents/firstmate` default lives in the
//! binary), so a caller can point a sampler at any firstmate checkout.
//!
//! CPU figures are deltas between consecutive samples, so a sampler's first
//! sample reads 0% everywhere; sample on a steady cadence (the TUI uses 2s).
//!
//! Sampling blocks: each refresh walks the whole process table and reads a
//! few small files per fleet row with synchronous filesystem I/O. From async
//! code, run it on a blocking thread (e.g. `tokio::task::spawn_blocking`) or a
//! dedicated sampler thread.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::activity::{load_task_state, Activity, StatusLine, TaskState};
use crate::detect::{build_sessions, Session};
use crate::gate::{load_gate_pass, GatePass};
use crate::meta::{find_by_cwd, load_fm_home, TaskRecord};
use crate::model::resolve_model;
use crate::procfs::{collect, core_percents, Snapshot};
use crate::prpoll::{load_pr_poll, PrPoll};
use crate::taskinfo::TaskInfo;
use crate::titles::{load_backlog_entries, load_task_titles_with, BacklogEntry, TaskTitles};

/// Fleet sampler over one firstmate home. See the module header.
pub struct Sampler {
    fm_home: PathBuf,
    records: Vec<TaskRecord>,
    titles: TaskTitles,
    backlog: Vec<BacklogEntry>,
    curr: Option<Snapshot>,
    prev: Option<Snapshot>,
    sessions: Vec<Session>,
    rows: Vec<AgentRow>,
}

/// One sample: the system overview plus one row per running agent session,
/// in the TUI's order (CPU% descending).
#[derive(Debug, Clone, Serialize)]
pub struct FleetSample {
    /// Unix time of the sample, in seconds.
    pub at: u64,
    pub system: SystemStats,
    pub agents: Vec<AgentRow>,
}

/// The TUI's system panel as numbers.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SystemStats {
    pub cores: usize,
    /// Busy percentage of each core since the previous sample.
    pub cpu_percent_per_core: Vec<f64>,
    pub mem_total_kib: u64,
    pub mem_used_kib: u64,
    pub swap_total_kib: u64,
    pub swap_used_kib: u64,
    /// 1-, 5- and 15-minute load averages.
    pub load_1m: f64,
    pub load_5m: f64,
    pub load_15m: f64,
    /// Total and currently running tasks (threads), as `/proc/loadavg`
    /// reports them.
    pub tasks: u64,
    pub tasks_running: u64,
    pub uptime_secs: u64,
}

/// One agent session, as a row of the TUI's agent table plus the fleet
/// signals behind its STATE column.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentRow {
    pub pid: i32,
    /// Runtime id from the detection table (`claude`, `opencode`, ...).
    pub kind: &'static str,
    /// Runtime label the TUI's RUNTIME column shows.
    pub runtime: &'static str,
    /// Display model from argv (`claude-opus-5-5[1m]`, `glm-5.2`), if any.
    pub model: Option<String>,
    /// Fleet task id and `state/<stem>.*` file stem, for fleet-matched rows.
    pub task_id: Option<String>,
    pub stem: Option<String>,
    /// Project basename of a fleet-matched row's record.
    pub project: Option<String>,
    /// The TASK column's text without its project prefix.
    pub title: String,
    /// A non-fleet session a human is driving.
    pub interactive: bool,
    pub activity: Activity,
    /// Last `.status` verb and its `[at=]` stamp, and the busy-state `ts=`
    /// stamp: the inputs a caller needs to judge how stale a row is.
    pub status_verb: Option<String>,
    pub status_at: Option<u64>,
    pub busy_ts: Option<u64>,
    /// CPU% over the session's subtree, one core = 100%.
    pub cpu_percent: f64,
    pub rss_kib: u64,
    pub elapsed_secs: u64,
    /// Processes in the session's subtree besides the agent itself.
    pub children: usize,
    /// The PR firstmate's merge poll is armed for.
    pub pr: Option<PrPoll>,
    /// From a fleet-matched row's `.meta`: the task kind (`ship`, `scout`,
    /// ...), its delivery mode and branch, the PR URL firstmate registered,
    /// and when its current incarnation was launched (Unix seconds).
    pub task_kind: Option<String>,
    pub mode: Option<String>,
    pub branch: Option<String>,
    pub registered_pr: Option<String>,
    pub spawned_at: Option<u64>,
    /// The task's whole `.status` log, oldest first.
    pub status_log: Vec<StatusLine>,
    /// The task's last green local gate.
    pub gate_pass: Option<GatePass>,
    /// The planner's estimate from the task's backlog entry, in minutes.
    pub estimate_min: Option<u64>,
    /// Modification time of `data/<task-id>/report.md`, when it exists.
    pub report_at: Option<u64>,
}

impl Sampler {
    /// A sampler over `fm_home`. Fleet records and titles are read now (and
    /// again on every refresh); a missing or unreadable home is not an error,
    /// it only leaves rows without fleet data.
    pub fn new(fm_home: PathBuf) -> Self {
        let records = load_fm_home(&fm_home);
        let backlog = load_backlog_entries(&fm_home);
        let titles = load_task_titles_with(&fm_home, &backlog);
        Self {
            fm_home,
            records,
            titles,
            backlog,
            curr: None,
            prev: None,
            sessions: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Refresh: re-read the fleet, take one process snapshot, rebuild the
    /// sessions and resolve each one.
    pub fn refresh(&mut self) {
        // Reload BEFORE resolving: this refresh's labels must come from this
        // refresh's records, not from whatever occupied a (recycled) worktree
        // earlier. See the `src/taskinfo.rs` header for why that matters.
        self.reload_fleet();

        let snap = collect();
        let mut sessions = build_sessions(&snap, self.curr.as_ref());
        self.rows = resolve_sessions(
            &self.fm_home,
            &self.records,
            &self.titles,
            &self.backlog,
            &snap,
            &mut sessions,
        );
        self.prev = self.curr.take();
        self.curr = Some(snap);
        self.sessions = sessions;
    }

    /// [`Sampler::refresh`], then the result as one serializable sample.
    /// Blocking; see the module header.
    pub fn sample(&mut self) -> FleetSample {
        self.refresh();
        FleetSample {
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            system: self
                .curr
                .as_ref()
                .map(|c| SystemStats::from_snapshots(c, self.prev.as_ref()))
                .unwrap_or_default(),
            agents: self.rows.clone(),
        }
    }

    /// Re-read firstmate fleet records and task titles from the home.
    /// Best-effort: a missing or unreadable source yields empty data, so the
    /// resolution layering falls back to the project-name / argv layers rather
    /// than retaining stale labels or propagating an error.
    fn reload_fleet(&mut self) {
        self.records = load_fm_home(&self.fm_home);
        self.backlog = load_backlog_entries(&self.fm_home);
        self.titles = load_task_titles_with(&self.fm_home, &self.backlog);
    }

    pub fn fm_home(&self) -> &Path {
        &self.fm_home
    }

    /// The fleet records read by the last refresh (or by [`Sampler::new`]).
    pub fn records(&self) -> &[TaskRecord] {
        &self.records
    }

    pub fn titles(&self) -> &TaskTitles {
        &self.titles
    }

    /// The backlog entries read by the last refresh.
    pub fn backlog(&self) -> &[BacklogEntry] {
        &self.backlog
    }

    /// The latest snapshot; `None` before the first refresh.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.curr.as_ref()
    }

    /// The snapshot before the latest one, which CPU deltas are taken against.
    pub fn prev_snapshot(&self) -> Option<&Snapshot> {
        self.prev.as_ref()
    }

    /// Sessions of the latest refresh, CPU% descending.
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Rows of the latest refresh, one per session and in the same order.
    pub fn rows(&self) -> &[AgentRow] {
        &self.rows
    }
}

impl SystemStats {
    /// System figures of `curr`, with per-core CPU taken against `prev`.
    pub fn from_snapshots(curr: &Snapshot, prev: Option<&Snapshot>) -> Self {
        let cpu_percent_per_core = core_percents(curr, prev);
        let mem = &curr.mem;
        Self {
            cores: cpu_percent_per_core.len(),
            cpu_percent_per_core,
            mem_total_kib: mem.mem_total_kib,
            mem_used_kib: mem.mem_total_kib.saturating_sub(mem.mem_avail_kib),
            swap_total_kib: mem.swap_total_kib,
            swap_used_kib: mem.swap_total_kib.saturating_sub(mem.swap_free_kib),
            load_1m: curr.load.one,
            load_5m: curr.load.five,
            load_15m: curr.load.fifteen,
            tasks: curr.load.total,
            tasks_running: curr.load.running,
            uptime_secs: curr.uptime.secs.max(0.0) as u64,
        }
    }
}

/// Fill each session's task, model and activity from the fleet and argv, and
/// return the matching rows. Rows come back one per session, in order.
fn resolve_sessions(
    fm_home: &Path,
    records: &[TaskRecord],
    titles: &TaskTitles,
    backlog: &[BacklogEntry],
    snap: &Snapshot,
    sessions: &mut [Session],
) -> Vec<AgentRow> {
    let mut rows = Vec::with_capacity(sessions.len());
    for s in sessions.iter_mut() {
        let mut record = None;
        let mut interactive = false;
        let mut state = TaskState::default();
        let mut pr = None;
        let mut gate_pass = None;
        if let Some(entry) = snap.procs.get(&s.pid) {
            let resolved = TaskInfo::resolve(
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
            s.task = resolved.line;
            s.task_project = resolved.project;
            s.model = resolve_model(&entry.cmdline).unwrap_or_default();
            interactive = resolved.interactive;
            // STATE column: fleet-matched rows classify from the task's
            // firstmate status/busy-state files (keyed by the record's
            // filename stem); non-fleet rows only know interactive vs
            // unknown.
            record = entry.cwd.as_deref().and_then(|c| find_by_cwd(records, c));
            match record {
                Some(r) => {
                    state = load_task_state(fm_home, &r.stem);
                    pr = load_pr_poll(fm_home, &r.stem);
                    gate_pass = load_gate_pass(fm_home, &r.stem);
                }
                None if interactive => state.activity = Activity::Interactive,
                None => state.activity = Activity::Unknown,
            }
            s.activity = state.activity;
        }
        rows.push(AgentRow {
            pid: s.pid,
            kind: s.kind.id,
            runtime: s.kind.display,
            model: (!s.model.is_empty()).then(|| s.model.clone()),
            task_id: record.map(|r| r.task_id.clone()),
            stem: record.map(|r| r.stem.clone()),
            project: s.task_project.clone(),
            title: s.task.clone(),
            interactive,
            activity: s.activity,
            status_verb: state.status_verb,
            status_at: state.status_at,
            busy_ts: state.busy_ts,
            cpu_percent: s.cpu_percent,
            rss_kib: s.rss_kib,
            elapsed_secs: s.elapsed_secs,
            children: s.children,
            pr,
            task_kind: record.and_then(|r| r.kind.clone()),
            mode: record.and_then(|r| r.mode.clone()),
            branch: record.and_then(|r| r.branch.clone()),
            registered_pr: record.and_then(|r| r.pr.clone()),
            spawned_at: record.and_then(TaskRecord::spawned_at),
            status_log: state.status_log,
            gate_pass,
            estimate_min: record
                .and_then(|r| backlog.iter().find(|e| e.id == r.task_id)?.estimate_min()),
            report_at: record.and_then(|r| report_mtime(fm_home, &r.task_id)),
        });
    }
    rows
}

/// Unix mtime of `data/<task-id>/report.md`, a scout's deliverable.
fn report_mtime(fm_home: &Path, task_id: &str) -> Option<u64> {
    if task_id.is_empty() || task_id.contains('/') || task_id.starts_with('.') {
        return None;
    }
    let path = fm_home.join("data").join(task_id).join("report.md");
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AGENT_KINDS;
    use crate::procfs::{CpuLine, LoadAvg, MemInfo, ProcEntry, ProcStat, Uptime, CLK_TZ};
    use std::fs;

    fn kind(id: &str) -> &'static crate::detect::AgentKind {
        AGENT_KINDS.iter().find(|k| k.id == id).unwrap()
    }

    /// A unique temp dir for this test run. Cleanup is best-effort (the OS
    /// reclaims the temp dir); only isolation within one test process
    /// matters. Mirrors the helper in `src/titles.rs`.
    fn tempfile_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "crew-watch-fleet-test-{}-{}-{}",
            label,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a `state/<task-id>.meta` record, with a `project=` line when
    /// `project` is given (which drops the `[task-id]` suffix from a titled
    /// task line).
    fn write_meta(home: &Path, task_id: &str, worktree: &str, project: Option<&str>) {
        let state = home.join("state");
        fs::create_dir_all(&state).unwrap();
        let project = project
            .map(|p| format!("project={p}\n"))
            .unwrap_or_default();
        fs::write(
            state.join(format!("{}.meta", task_id)),
            format!(
                "endpoint_task_id={}\nworktree={}\n{}harness=opencode\n",
                task_id, worktree, project
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

    fn agent_snapshot(cwd: &str, cmdline: &[&str]) -> Snapshot {
        let mut snap = Snapshot {
            tick_hz: CLK_TZ,
            uptime: Uptime { secs: 10_000.0 },
            ..Snapshot::default()
        };
        snap.procs.insert(
            42,
            ProcEntry {
                stat: ProcStat {
                    pid: 42,
                    ppid: 1,
                    comm: cmdline[0].to_string(),
                    rss_pages: 256,
                    // Started 100s after boot: 9900s before the 10000s uptime.
                    starttime: 100 * CLK_TZ,
                    ..ProcStat::default()
                },
                cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
                cwd: Some(PathBuf::from(cwd)),
            },
        );
        // One tool process in the agent's subtree.
        snap.procs.insert(
            43,
            ProcEntry {
                stat: ProcStat {
                    pid: 43,
                    ppid: 42,
                    comm: "cargo".to_string(),
                    rss_pages: 128,
                    ..ProcStat::default()
                },
                cmdline: vec!["cargo".to_string()],
                cwd: Some(PathBuf::from(cwd)),
            },
        );
        snap
    }

    fn resolve(home: &Path, snap: &Snapshot) -> (Vec<Session>, Vec<AgentRow>) {
        let records = load_fm_home(home);
        let titles = crate::titles::load_task_titles(home);
        let mut sessions = build_sessions(snap, None);
        let backlog = load_backlog_entries(home);
        let rows = resolve_sessions(home, &records, &titles, &backlog, snap, &mut sessions);
        (sessions, rows)
    }

    // --- rows ---

    #[test]
    fn fleet_row_carries_task_state_stamps_and_pr() {
        let home = tempfile_dir("fleet-row");
        write_meta(
            &home,
            "cw-lib",
            WORKTREE,
            Some("/home/crew/projects/firstmate"),
        );
        write_backlog(
            &home,
            "- [ ] cw-lib - split crew-watch into a library (repo: x)\n",
        );
        let state = home.join("state");
        fs::write(
            state.join("cw-lib.status"),
            "working [at=1793381000]: setup\npaused [at=1793381955]: awaiting review\n",
        )
        .unwrap();
        fs::write(
            state.join("cw-lib.pr-poll"),
            "github\nhttps://github.com/o/r/pull/7\ngithub.com\no/r\n7\n",
        )
        .unwrap();

        let snap = agent_snapshot(WORKTREE, &["opencode", "--model", "zai/glm-5.2"]);
        let (sessions, rows) = resolve(&home, &snap);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.pid, 42);
        assert_eq!((row.kind, row.runtime), ("opencode", "opencode"));
        assert_eq!(row.model.as_deref(), Some("glm-5.2"));
        assert_eq!(row.task_id.as_deref(), Some("cw-lib"));
        assert_eq!(row.stem.as_deref(), Some("cw-lib"));
        assert_eq!(row.project.as_deref(), Some("firstmate"));
        assert_eq!(row.title, "split crew-watch into a library");
        assert!(!row.interactive);
        assert_eq!(row.activity, Activity::Paused);
        assert_eq!(row.status_verb.as_deref(), Some("paused"));
        assert_eq!(row.status_at, Some(1_793_381_955));
        assert_eq!(row.busy_ts, None);
        // The subtree: the agent and its one child, 256 + 128 pages of 4 KiB.
        assert_eq!(row.rss_kib, 1536);
        assert_eq!(row.children, 1);
        assert_eq!(row.elapsed_secs, 9900);
        assert_eq!(row.pr.as_ref().map(|p| p.number), Some(7));
        // The TUI's session is filled from the same resolution, and the
        // row's process figures are the session's.
        let s = &sessions[0];
        assert_eq!(s.task, row.title);
        assert_eq!(s.activity, Activity::Paused);
        assert_eq!(
            (row.cpu_percent, row.rss_kib, row.elapsed_secs, row.children),
            (s.cpu_percent, s.rss_kib, s.elapsed_secs, s.children)
        );

        let json = serde_json::to_value(row).unwrap();
        assert_eq!(json["activity"], "paused");
        assert_eq!(json["pr"]["url"], "https://github.com/o/r/pull/7");
        // A meta without the task-shape keys leaves them empty.
        assert_eq!((row.task_kind.as_ref(), row.spawned_at), (None, None));
        assert_eq!(row.status_log.len(), 2);
        assert_eq!(row.status_log[1].text, "awaiting review");
        assert_eq!(row.gate_pass, None);
        assert_eq!(row.estimate_min, None);
        assert_eq!(row.report_at, None);
    }

    #[test]
    fn fleet_row_carries_the_tasks_shape_gate_estimate_and_report() {
        let home = tempfile_dir("fleet-shape");
        let state = home.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("tl.meta"),
            format!(
                "endpoint_task_id=tl\nworktree={WORKTREE}\nkind=ship\nmode=direct-PR\n\
                 branch=fm/tl\npr=https://github.com/o/r/pull/8\nspawn_gen=s1793440277.26.77\n"
            ),
        )
        .unwrap();
        fs::write(
            state.join("tl.status"),
            "working [at=1793440300]: setup\n\
             paused [at=1793440900]: awaiting pre-push review of fm/tl at 93d577a\n",
        )
        .unwrap();
        let sha = "93d577ab0c7124172e1414f7d943252f9bc961f9";
        fs::write(
            state.join("tl.gate-pass"),
            format!("head={sha} ts=1793440890\n"),
        )
        .unwrap();
        write_backlog(
            &home,
            "- [ ] other - another task (repo: x) (kind: ship)\n  estimate=90m\n\
             - [ ] tl - timelines (repo: miles) (kind: ship)\n  plan: estimate=35m\n",
        );
        fs::create_dir_all(home.join("data").join("tl")).unwrap();
        fs::write(home.join("data").join("tl").join("report.md"), "# r\n").unwrap();

        let snap = agent_snapshot(WORKTREE, &["claude"]);
        let (_, rows) = resolve(&home, &snap);
        let row = &rows[0];
        assert_eq!(row.task_kind.as_deref(), Some("ship"));
        assert_eq!(row.mode.as_deref(), Some("direct-PR"));
        assert_eq!(row.branch.as_deref(), Some("fm/tl"));
        assert_eq!(
            row.registered_pr.as_deref(),
            Some("https://github.com/o/r/pull/8")
        );
        assert_eq!(row.spawned_at, Some(1_793_440_277));
        let verbs: Vec<&str> = row.status_log.iter().map(|l| l.verb.as_str()).collect();
        assert_eq!(verbs, ["working", "paused"]);
        assert_eq!(row.status_log[1].at, Some(1_793_440_900));
        assert_eq!(
            row.gate_pass,
            Some(GatePass {
                head: sha.to_string(),
                ts: Some(1_793_440_890)
            })
        );
        assert_eq!(row.estimate_min, Some(35));
        assert!(row.report_at.is_some_and(|t| t > 1_700_000_000));

        let json = serde_json::to_value(row).unwrap();
        assert_eq!(json["task_kind"], "ship");
        assert_eq!(json["status_log"][1]["verb"], "paused");
        assert_eq!(json["gate_pass"]["head"], sha);
    }

    #[test]
    fn non_fleet_rows_have_no_fleet_fields() {
        let home = tempfile_dir("non-fleet");
        let snap = agent_snapshot("/home/crew/scratch", &["claude"]);
        let (_, rows) = resolve(&home, &snap);
        let row = &rows[0];
        assert_eq!(row.activity, Activity::Interactive);
        assert!(row.interactive);
        assert_eq!(row.title, "interactive @ scratch");
        assert_eq!(row.task_id, None);
        assert_eq!(row.model, None);
        assert_eq!(row.status_verb, None);
        assert_eq!(row.pr, None);

        // A prompt in argv marks it autonomous: no interactive glyph.
        let snap = agent_snapshot("/home/crew/scratch", &["claude", "-p", "do the thing"]);
        let (_, rows) = resolve(&home, &snap);
        assert_eq!(rows[0].activity, Activity::Unknown);
        assert!(!rows[0].interactive);
    }

    // --- system stats ---

    #[test]
    fn system_stats_derive_used_memory_and_per_core_cpu() {
        let line = |name: &str, busy: u64, idle: u64| CpuLine {
            name: name.to_string(),
            fields: vec![busy, 0, 0, idle, 0],
        };
        let prev = Snapshot {
            cpus: vec![line("cpu", 0, 0), line("cpu0", 0, 0), line("cpu1", 0, 0)],
            ..Snapshot::default()
        };
        let curr = Snapshot {
            cpus: vec![
                line("cpu", 0, 0),
                line("cpu0", 50, 50),
                line("cpu1", 0, 100),
            ],
            mem: MemInfo {
                mem_total_kib: 1000,
                mem_avail_kib: 400,
                swap_total_kib: 200,
                swap_free_kib: 150,
            },
            load: LoadAvg {
                one: 1.5,
                five: 1.0,
                fifteen: 0.5,
                running: 3,
                total: 900,
            },
            uptime: Uptime { secs: 3600.7 },
            ..Snapshot::default()
        };
        let stats = SystemStats::from_snapshots(&curr, Some(&prev));
        assert_eq!(
            stats,
            SystemStats {
                cores: 2,
                cpu_percent_per_core: vec![50.0, 0.0],
                mem_total_kib: 1000,
                mem_used_kib: 600,
                swap_total_kib: 200,
                swap_used_kib: 50,
                load_1m: 1.5,
                load_5m: 1.0,
                load_15m: 0.5,
                tasks: 900,
                tasks_running: 3,
                uptime_secs: 3600,
            }
        );
    }

    #[test]
    fn sample_reads_the_live_system() {
        let mut sampler = Sampler::new(PathBuf::from("/nonexistent/fm-home-fleet-test"));
        assert!(sampler.snapshot().is_none());
        let first = sampler.sample();
        let second = sampler.sample();
        assert!(second.at >= first.at);
        assert!(second.system.cores >= 1);
        assert!(second.system.mem_total_kib > 0);
        assert_eq!(sampler.rows().len(), sampler.sessions().len());
        assert!(sampler.prev_snapshot().is_some());
    }

    // --- reload_fleet: fleet freshness across refreshes ---

    #[test]
    fn reload_picks_up_recycled_worktree_new_task() {
        // The observed bug: crew-watch loaded fleet state once at launch, then
        // a worker copy was recycled from a finished task to a new one in the
        // SAME reused worktree path. The stale snapshot still attributed that
        // path to the finished task, so the new agent was mislabelled. Here a
        // fresh Sampler is built against task-A, the on-disk record is then
        // swapped to task-B at the identical worktree, and reload_fleet must
        // make resolution converge on task-B's title.
        let home = tempfile_dir("recycled");
        write_meta(&home, "finished-task", WORKTREE, None);
        write_backlog(
            &home,
            "- [x] finished-task - give each worker its own browser session (repo: x) (done 2026-08-12)\n\
             - [ ] fresh-scout - scout the new fleet layout (repo: x) (kind: ship)\n",
        );

        let mut sampler = Sampler::new(home.clone());
        let cwd = PathBuf::from(WORKTREE);
        let cmdline = vec!["opencode".to_string()];

        // At construction the worktree is still occupied by finished-task.
        let initial = TaskInfo::resolve(
            sampler.records(),
            sampler.titles(),
            Some(&cwd),
            || None,
            &cmdline,
            kind("opencode"),
        );
        assert_eq!(
            initial.line,
            "give each worker its own browser session [finished-task]"
        );

        // Firstmate recycles the worktree: finished-task's meta is removed and
        // fresh-scout takes over the SAME path.
        fs::remove_file(home.join("state").join("finished-task.meta")).unwrap();
        write_meta(&home, "fresh-scout", WORKTREE, None);

        // reload_fleet is exactly what refresh() calls before resolving.
        sampler.reload_fleet();

        let after = TaskInfo::resolve(
            sampler.records(),
            sampler.titles(),
            Some(&cwd),
            || None,
            &cmdline,
            kind("opencode"),
        );
        assert_eq!(
            after.line, "scout the new fleet layout [fresh-scout]",
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
        write_meta(&home, "doomed-task", WORKTREE, None);
        write_backlog(
            &home,
            "- [ ] doomed-task - soon to be torn down (repo: x)\n",
        );

        let mut sampler = Sampler::new(home.clone());
        let cwd = PathBuf::from(WORKTREE);
        assert!(find_by_cwd(sampler.records(), &cwd).is_some());

        // Task is torn down; no replacement reuses the worktree.
        fs::remove_file(home.join("state").join("doomed-task.meta")).unwrap();
        sampler.reload_fleet();

        assert!(
            find_by_cwd(sampler.records(), &cwd).is_none(),
            "no ghost record after teardown"
        );
        // And resolution no longer claims the dead task id.
        let line = TaskInfo::resolve(
            sampler.records(),
            sampler.titles(),
            Some(&cwd),
            || Some("firstmate".to_string()),
            &["opencode".to_string()],
            kind("opencode"),
        );
        assert!(
            !line.line.contains("doomed-task"),
            "fallen-through line must not carry the dead task id, got: {:?}",
            line.line
        );
    }

    // --- refresh(): the public path re-reads the fleet every time ---

    #[test]
    fn refresh_drops_a_record_removed_since_the_last_one() {
        let home = tempfile_dir("refresh-records");
        write_meta(&home, "torn-down", WORKTREE, None);
        let mut sampler = Sampler::new(home.clone());
        assert_eq!(sampler.records().len(), 1);

        fs::remove_file(home.join("state").join("torn-down.meta")).unwrap();
        sampler.refresh();
        assert!(sampler.records().is_empty(), "refresh must re-read records");
    }

    #[test]
    fn refresh_picks_up_a_retitled_task() {
        let home = tempfile_dir("refresh-titles");
        write_backlog(&home, "- [ ] retitled - the old title (repo: x)\n");
        let mut sampler = Sampler::new(home.clone());
        assert_eq!(
            sampler.titles().title_for("retitled"),
            Some("the old title")
        );

        write_backlog(&home, "- [ ] retitled - the new title (repo: x)\n");
        let sample = sampler.sample();
        assert_eq!(
            sampler.titles().title_for("retitled"),
            Some("the new title")
        );
        assert_eq!(sample.agents, sampler.rows());
    }

    // --- non-fatal sources ---

    #[test]
    fn construct_with_missing_fm_home_is_non_fatal() {
        // A sampler may be pointed at a home that does not exist yet.
        // Construction must succeed and yield empty records/titles.
        let sampler = Sampler::new(PathBuf::from("/nonexistent/fm-home-fleet-test-99999"));
        assert!(sampler.records().is_empty());
    }

    #[test]
    fn reload_survives_vanished_fm_home() {
        // A fleet home that vanishes mid-run must never crash the monitor.
        // reload_fleet re-reads best-effort and yields empty data.
        let home = tempfile_dir("vanish-midrun");
        write_meta(&home, "some-task", WORKTREE, None);

        let mut sampler = Sampler::new(home.clone());
        assert_eq!(sampler.records().len(), 1);

        // Wipe the whole home out from under the running monitor.
        fs::remove_dir_all(&home).unwrap();
        sampler.reload_fleet();

        assert!(
            sampler.records().is_empty(),
            "vanished home yields empty, no panic"
        );
    }
}
