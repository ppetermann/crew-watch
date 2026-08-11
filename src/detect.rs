//! Agent-runtime detection and per-session subtree aggregation.
//!
//! Detection is a single table ([`AGENT_KINDS`]) of patterns matched against a
//! process's command name (basename of argv[0], with a fallback through known
//! interpreters like `node`). Adding a new runtime is one table row.
//!
//! Aggregation attributes every process to its *nearest enclosing agent*:
//! - each detected agent becomes one row (a "session"), and
//! - a nested agent (an agent whose ancestor chain contains another agent) is
//!   listed as its own row and excluded from the ancestor's aggregate.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::procfs::Snapshot;

/// One matcher against a command basename.
#[derive(Debug, Clone, Copy)]
pub enum Match {
    /// Basename equals this string exactly.
    Exact(&'static str),
    /// Basename begins with this prefix (handles versioned/wrapper binaries
    /// like `muse-bin-<version>` or `pi-launcher`).
    Prefix(&'static str),
}

impl Match {
    fn matches(&self, name: &str) -> bool {
        match self {
            Match::Exact(s) => name == *s,
            Match::Prefix(s) => name.starts_with(s),
        }
    }
}

/// A detectable agent runtime. Add a row here to support a new runtime.
#[derive(Debug, Clone, Copy)]
pub struct AgentKind {
    pub id: &'static str,
    pub display: &'static str,
    pub matches: &'static [Match],
}

/// The detection table. Order is irrelevant: a process matches at most one
/// runtime in practice.
pub const AGENT_KINDS: &[AgentKind] = &[
    AgentKind {
        id: "claude",
        display: "claude",
        matches: &[Match::Exact("claude"), Match::Prefix("claude-")],
    },
    AgentKind {
        id: "opencode",
        display: "opencode",
        matches: &[Match::Exact("opencode"), Match::Prefix("opencode-")],
    },
    AgentKind {
        id: "codex",
        display: "codex",
        matches: &[Match::Exact("codex"), Match::Prefix("codex-")],
    },
    AgentKind {
        id: "grok",
        display: "grok",
        matches: &[Match::Exact("grok"), Match::Prefix("grok-")],
    },
    AgentKind {
        id: "kimi",
        display: "kimi",
        matches: &[Match::Exact("kimi"), Match::Prefix("kimi-")],
    },
    AgentKind {
        id: "muse",
        display: "muse",
        // `muse` launchers exec `muse-bin-<version>`; `muse-*` covers both.
        matches: &[Match::Exact("muse"), Match::Prefix("muse-")],
    },
    AgentKind {
        id: "pi",
        display: "pi",
        // `pi` and `pi-launcher`. Exact "pi" avoids matching `pip`, `ping`, ...
        matches: &[Match::Exact("pi"), Match::Prefix("pi-")],
    },
];

/// Interpreter basenames whose first non-flag argument is the real program
/// path (e.g. `node /opt/claude/code`).
const INTERPRETERS: &[&str] = &["node", "deno", "bun", "bunx", "python", "python3", "ruby"];

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Build candidate command basenames from argv. Includes argv[0]'s basename,
/// and when argv[0] is an interpreter, the first following non-flag argument's
/// basename too.
pub fn extract_candidates(cmdline: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(a0) = cmdline.first() else {
        return out;
    };
    let b = basename(a0);
    out.push(b.clone());
    if INTERPRETERS.contains(&b.as_str()) {
        for tok in cmdline.iter().skip(1) {
            if !tok.starts_with('-') {
                out.push(basename(tok));
                break;
            }
        }
    }
    out
}

/// Detect which agent runtime a command line belongs to.
pub fn detect_kind(cmdline: &[String]) -> Option<&'static AgentKind> {
    let cands = extract_candidates(cmdline);
    for kind in AGENT_KINDS {
        for matcher in kind.matches {
            for cand in &cands {
                if matcher.matches(cand) {
                    return Some(kind);
                }
            }
        }
    }
    None
}

/// One agent session row.
#[derive(Debug, Clone)]
pub struct Session {
    pub kind: &'static AgentKind,
    pub pid: i32,
    /// Aggregated CPU% over the whole attributed subtree, one-core = 100%
    /// (so a multi-core subtree can exceed 100%, matching htop).
    pub cpu_percent: f64,
    /// Aggregated resident memory in KiB over the attributed subtree.
    pub rss_kib: u64,
    /// Elapsed seconds of the session's root agent process.
    pub elapsed_secs: u64,
    /// Resolved one-line "what is it working on" text (filled by the caller
    /// via [`crate::taskinfo`]).
    pub task: String,
    /// Compact display model parsed from argv (e.g. `glm-5.2`), or empty when
    /// no model flag was present (filled by the caller; rendered as `-`).
    pub model: String,
}

/// Walk strict parents of `pid` until reaching a detected agent, or run out.
/// Cycle-guarded against pathological ppid loops.
fn nearest_agent_ancestor(
    snap: &Snapshot,
    agents: &HashMap<i32, &'static AgentKind>,
    start: i32,
) -> Option<i32> {
    let mut pid = start;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(pid) {
            return None;
        }
        let parent = snap.procs.get(&pid).map(|e| e.stat.ppid).unwrap_or(0);
        if parent == 0 {
            return None;
        }
        if agents.contains_key(&parent) {
            return Some(parent);
        }
        pid = parent;
    }
}

/// Build the list of agent sessions from a snapshot, using the previous tick's
/// snapshot to compute CPU deltas.
pub fn build_sessions(curr: &Snapshot, prev: Option<&Snapshot>) -> Vec<Session> {
    // 1. Detect every agent process.
    let mut agents: HashMap<i32, &'static AgentKind> = HashMap::new();
    for (&pid, entry) in &curr.procs {
        let mut kind = detect_kind(&entry.cmdline);
        // Kernel threads have no cmdline; fall back to comm as a last resort.
        if kind.is_none() && entry.cmdline.is_empty() && !entry.stat.comm.is_empty() {
            kind = detect_kind(std::slice::from_ref(&entry.stat.comm));
        }
        if let Some(k) = kind {
            agents.insert(pid, k);
        }
    }
    if agents.is_empty() {
        return Vec::new();
    }

    // 2. Attribute every process to its nearest enclosing agent (or itself if
    //    it is an agent). This naturally splits nested agents from ancestors.
    let mut owner_of: HashMap<i32, i32> = HashMap::new();
    for &pid in curr.procs.keys() {
        let owner = if agents.contains_key(&pid) {
            pid
        } else {
            nearest_agent_ancestor(curr, &agents, pid).unwrap_or(0)
        };
        if owner != 0 {
            owner_of.insert(pid, owner);
        }
    }

    // 3. Aggregate resident memory (from curr) and CPU deltas (curr vs prev)
    //    per owner.
    let mut rss: HashMap<i32, u64> = HashMap::new();
    let mut ticks: HashMap<i32, u64> = HashMap::new();
    for (&pid, entry) in &curr.procs {
        let Some(&owner) = owner_of.get(&pid) else {
            continue;
        };
        *rss.entry(owner).or_default() += entry.stat.rss_kib();
        if let Some(p) = prev {
            if let Some(pe) = p.procs.get(&pid) {
                let d = entry
                    .stat
                    .utime
                    .saturating_sub(pe.stat.utime)
                    .saturating_add(entry.stat.stime.saturating_sub(pe.stat.stime));
                *ticks.entry(owner).or_default() += d;
            }
        }
    }

    // 4. Compute CPU%. total machine jiffies over the interval come from the
    //    aggregate `cpu` line; per-core normalization makes 100% = one core.
    let num_cores = curr
        .cpus
        .iter()
        .filter(|c| !c.is_aggregate())
        .count()
        .max(1) as f64;
    let total_delta = prev
        .and_then(|p| {
            let ca = curr.cpus.iter().find(|c| c.is_aggregate());
            let pa = p.cpus.iter().find(|c| c.is_aggregate());
            match (ca, pa) {
                (Some(c), Some(pn)) => Some(c.total().saturating_sub(pn.total())),
                _ => None,
            }
        })
        .unwrap_or(0);

    let mut sessions = Vec::with_capacity(agents.len());
    for (&pid, &kind) in &agents {
        let Some(entry) = curr.procs.get(&pid) else {
            continue;
        };
        let cpu_percent = if total_delta > 0 {
            let dt = ticks.get(&pid).copied().unwrap_or(0) as f64;
            dt / total_delta as f64 * 100.0 * num_cores
        } else {
            0.0
        };
        let start_secs = entry.stat.starttime as f64 / curr.tick_hz.max(1) as f64;
        let elapsed_secs = (curr.uptime.secs - start_secs).max(0.0) as u64;
        sessions.push(Session {
            kind,
            pid,
            cpu_percent,
            rss_kib: rss.get(&pid).copied().unwrap_or(0),
            elapsed_secs,
            task: String::new(),
            model: String::new(),
        });
    }

    sessions.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(Ordering::Equal)
    });
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::{CpuLine, ProcEntry, ProcStat, Uptime, CLK_TZ};

    fn agent(pid: i32, ppid: i32, comm: &str, cmdline: &[&str]) -> ProcEntry {
        ProcEntry {
            stat: ProcStat {
                pid,
                ppid,
                comm: comm.to_string(),
                utime: 100,
                stime: 50,
                rss_pages: 1024,
                ..Default::default()
            },
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            cwd: None,
        }
    }

    fn proc_(pid: i32, ppid: i32, comm: &str) -> ProcEntry {
        ProcEntry {
            stat: ProcStat {
                pid,
                ppid,
                comm: comm.to_string(),
                utime: 100,
                stime: 50,
                rss_pages: 512,
                ..Default::default()
            },
            cmdline: vec![comm.to_string()],
            cwd: None,
        }
    }

    /// Build a snapshot with a synthetic 2-core cpu topology whose aggregate
    /// `cpu` line total is `agg_total * 8` (8 fields per line).
    fn snap(procs: Vec<(i32, ProcEntry)>, agg_total: u64) -> Snapshot {
        let mut s = Snapshot {
            tick_hz: CLK_TZ,
            uptime: Uptime { secs: 10_000.0 },
            cpus: vec![
                CpuLine {
                    name: "cpu".to_string(),
                    fields: vec![agg_total; 8],
                },
                CpuLine {
                    name: "cpu0".to_string(),
                    fields: vec![agg_total / 2; 8],
                },
                CpuLine {
                    name: "cpu1".to_string(),
                    fields: vec![agg_total / 2; 8],
                },
            ],
            ..Default::default()
        };
        for (pid, e) in procs {
            s.procs.insert(pid, e);
        }
        s
    }

    // --- detection table ---

    #[test]
    fn detect_direct_basenames() {
        assert_eq!(
            detect_kind(&["claude".to_string()]).map(|k| k.id),
            Some("claude")
        );
        assert_eq!(
            detect_kind(&["opencode".to_string()]).map(|k| k.id),
            Some("opencode")
        );
    }

    #[test]
    fn detect_versioned_and_wrappers() {
        assert_eq!(
            detect_kind(&["muse-bin-1.2.3".to_string()]).map(|k| k.id),
            Some("muse")
        );
        assert_eq!(
            detect_kind(&["pi-launcher".to_string()]).map(|k| k.id),
            Some("pi")
        );
        assert_eq!(
            detect_kind(&["/usr/bin/opencode".to_string()]).map(|k| k.id),
            Some("opencode")
        );
    }

    #[test]
    fn detect_through_interpreter() {
        // node-launched: argv[1]'s basename is the runtime name.
        assert_eq!(
            detect_kind(&["node".to_string(), "/opt/claude/claude".to_string()]).map(|k| k.id),
            Some("claude")
        );
    }

    #[test]
    fn detect_rejects_lookalikes() {
        // `pi` exact must not match `pip` / `ping`.
        assert!(detect_kind(&["pip".to_string()]).is_none());
        assert!(detect_kind(&["ping".to_string()]).is_none());
        assert!(detect_kind(&["cargo".to_string()]).is_none());
    }

    // --- aggregation ---

    #[test]
    fn subtree_aggregates_into_one_session() {
        // claude(1) -> cargo(2) -> cc(3)
        let procs = vec![
            (1, agent(1, 0, "claude", &["claude"])),
            (2, proc_(2, 1, "cargo")),
            (3, proc_(3, 2, "cc")),
        ];
        let curr = snap(procs.clone(), 2000);
        // prev: same tree with 10 fewer utime jiffies per pid, and a smaller
        // aggregate cpu total so the delta is nonzero.
        let mut prev = snap(procs, 1000);
        for e in prev.procs.values_mut() {
            e.stat.utime -= 10;
        }
        let sessions = build_sessions(&curr, Some(&prev));
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.kind.id, "claude");
        assert_eq!(s.pid, 1);
        // rss: 1024 + 512 + 512 pages * 4 KiB
        assert_eq!(s.rss_kib, (1024 + 512 + 512) * 4);
        // delta = 10 per pid * 3 pids = 30; total_delta = 8*(2000-1000) = 8000;
        // 2 cores -> 30 / 8000 * 100 * 2 = 0.75%
        assert!((s.cpu_percent - 0.75).abs() < 1e-6, "got {}", s.cpu_percent);
    }

    #[test]
    fn nested_agent_is_separate_and_excluded_from_ancestor() {
        // opencode(1) -> claude(5) -> bash(6); plus cargo(2) under opencode.
        let procs = vec![
            (1, agent(1, 0, "opencode", &["opencode"])),
            (2, proc_(2, 1, "cargo")),
            (5, agent(5, 1, "claude", &["claude"])),
            (6, proc_(6, 5, "bash")),
        ];
        let curr = snap(procs.clone(), 2000);
        let mut prev = snap(procs, 1000);
        for e in prev.procs.values_mut() {
            e.stat.utime -= 10;
        }
        let sessions = build_sessions(&curr, Some(&prev));
        assert_eq!(sessions.len(), 2, "two sessions");
        let by_id: HashMap<&str, &Session> = sessions.iter().map(|s| (s.kind.id, s)).collect();
        let oc = by_id.get("opencode").unwrap();
        let cl = by_id.get("claude").unwrap();
        // opencode owns {1,2}; claude owns {5,6}. Ancestor does NOT include child agent.
        assert_eq!(oc.rss_kib, (1024 + 512) * 4);
        assert_eq!(cl.rss_kib, (1024 + 512) * 4);
    }

    #[test]
    fn no_agents_returns_empty() {
        let procs = vec![(1, proc_(1, 0, "bash"))];
        let curr = snap(procs, 2000);
        assert!(build_sessions(&curr, None).is_empty());
    }

    #[test]
    fn first_tick_has_zero_cpu() {
        let procs = vec![(1, agent(1, 0, "claude", &["claude"]))];
        let curr = snap(procs, 2000);
        let sessions = build_sessions(&curr, None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cpu_percent, 0.0);
    }

    #[test]
    fn sorted_by_cpu_desc() {
        let procs = vec![
            (1, agent(1, 0, "claude", &["claude"])),
            (2, agent(2, 0, "opencode", &["opencode"])),
        ];
        let curr = snap(procs.clone(), 2000);
        let mut prev = snap(procs, 1000);
        // Make pid 2 (opencode) hotter than pid 1 (claude).
        prev.procs.get_mut(&1).unwrap().stat.utime -= 5; // delta 5
        prev.procs.get_mut(&2).unwrap().stat.utime -= 80; // delta 80
        let sessions = build_sessions(&curr, Some(&prev));
        assert_eq!(sessions.len(), 2);
        assert!(sessions[0].cpu_percent >= sessions[1].cpu_percent);
        assert_eq!(sessions[0].kind.id, "opencode");
    }
}
