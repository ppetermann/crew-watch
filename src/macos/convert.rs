//! Pure conversion layer for the macOS backend: plain sysinfo values to
//! `procfs` snapshot types, with no sysinfo types in scope so the math is
//! unit-testable on every platform.

use crate::procfs::{CpuLine, ProcEntry, ProcStat};

/// Fabricated cumulative per-core tick counters, advanced from sysinfo's
/// per-core usage% and wall-clock elapsed time.
///
/// sysinfo exposes only per-core *usage %*, not cumulative ticks; the
/// downstream consumers (`detect.rs` CPU deltas, `ui.rs` per-core meters)
/// speak the Linux `/proc/stat` dialect of cumulative counters. Each
/// `advance` integrates usage% × elapsed × hz into `user`/`idle` buckets so
/// the deltas computed between two snapshots reproduce the usage% exactly.
pub struct CpuAccum {
    user: Vec<f64>,
    idle: Vec<f64>,
}

impl CpuAccum {
    pub fn new(ncores: usize) -> Self {
        Self {
            user: vec![0.0; ncores],
            idle: vec![0.0; ncores],
        }
    }

    /// Per core i: busy += elapsed*hz*pct[i]/100; idle += elapsed*hz*(1-pct[i]/100).
    /// Resizes (zero-filled) if the core count changes.
    pub fn advance(&mut self, elapsed_secs: f64, usage_pct: &[f32], tick_hz: u64) {
        if usage_pct.len() != self.user.len() {
            self.user.resize(usage_pct.len(), 0.0);
            self.idle.resize(usage_pct.len(), 0.0);
        }
        let total = elapsed_secs * tick_hz as f64;
        for (i, &pct) in usage_pct.iter().enumerate() {
            let busy = total * f64::from(pct) / 100.0;
            self.user[i] += busy;
            self.idle[i] += total - busy;
        }
    }

    /// Linux field order [user, nice=0, sys=0, idle, iowait=0]; aggregate
    /// "cpu" line (sums) first, then "cpu0".."cpuN".
    pub fn to_cpu_lines(&self) -> Vec<CpuLine> {
        let round_sum = |vs: &[f64]| -> u64 { vs.iter().map(|v| v.round() as u64).sum() };
        let mut lines = Vec::with_capacity(self.user.len() + 1);
        lines.push(CpuLine {
            name: "cpu".to_string(),
            fields: vec![round_sum(&self.user), 0, 0, round_sum(&self.idle), 0],
        });
        for i in 0..self.user.len() {
            lines.push(CpuLine {
                name: format!("cpu{i}"),
                fields: vec![
                    self.user[i].round() as u64,
                    0,
                    0,
                    self.idle[i].round() as u64,
                    0,
                ],
            });
        }
        lines
    }
}

/// (uptime_secs - run_secs).max(0) * tick_hz — round-trips through
/// detect.rs's elapsed formula so ELAPSED == sysinfo's run_time exactly.
pub fn starttime_ticks(uptime_secs: f64, run_secs: u64, tick_hz: u64) -> u64 {
    ((uptime_secs - run_secs as f64).max(0.0) * tick_hz as f64).round() as u64
}

/// Plain-value process row, decoupled from sysinfo types so conversion is
/// unit-testable off-mac.
pub struct RawProc {
    pub pid: i32,
    pub ppid: i32,
    pub name: String,
    pub accumulated_cpu_ms: u64,
    pub mem_bytes: u64,
    pub run_secs: u64,
    pub cmdline: Vec<String>,
    pub cwd: Option<std::path::PathBuf>,
}

/// utime = ms/10 (centiseconds == ticks at hz 100); stime = 0 (macOS gives
/// one combined counter; detect.rs sums both, so the split is irrelevant);
/// rss_pages = mem_bytes / 4096 (keeps rss_kib() == bytes/1024).
pub fn to_proc_entry(raw: RawProc, uptime_secs: f64, tick_hz: u64) -> ProcEntry {
    ProcEntry {
        stat: ProcStat {
            pid: raw.pid,
            ppid: raw.ppid,
            comm: raw.name,
            utime: raw.accumulated_cpu_ms / 10,
            stime: 0,
            starttime: starttime_ticks(uptime_secs, raw.run_secs, tick_hz),
            rss_pages: raw.mem_bytes / 4096,
        },
        cmdline: raw.cmdline,
        cwd: raw.cwd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::build_sessions;
    use crate::procfs::{Snapshot, Uptime, CLK_TZ};

    #[test]
    fn cpu_accum_two_advances_reproduce_usage() {
        let hz = 100u64;
        let mut accum = CpuAccum::new(2);
        accum.advance(2.0, &[25.0, 25.0], hz);
        let first = accum.to_cpu_lines();
        assert_eq!(first.len(), 3, "aggregate + 2 cores");
        assert!(first[0].is_aggregate());
        assert_eq!(first[0].name, "cpu");
        assert_eq!(first[1].name, "cpu0");
        assert_eq!(first[2].name, "cpu1");
        // Per core: busy 2.0*100*0.25 = 50, idle 150.
        assert_eq!(first[1].fields, vec![50, 0, 0, 150, 0]);
        // Aggregate line is the sums.
        assert_eq!(first[0].fields, vec![100, 0, 0, 300, 0]);

        accum.advance(2.0, &[75.0, 75.0], hz);
        let second = accum.to_cpu_lines();
        assert_eq!(second[1].fields, vec![200, 0, 0, 200, 0]);
        // Between the two renders, per-core busy fraction = Δtotal-Δidle over
        // Δtotal must reproduce usage 75% within 1 tick.
        let dt = second[1].total() - first[1].total();
        let di = second[1].idle() - first[1].idle();
        let busy_frac = 1.0 - di as f64 / dt as f64;
        assert!(
            (busy_frac - 0.75).abs() * dt as f64 <= 1.0,
            "busy_frac {busy_frac} not 0.75 within 1 tick"
        );
    }

    #[test]
    fn idle_lands_in_linux_field_order() {
        // fields[3]+fields[4] must equal the idle ticks fed in.
        let mut accum = CpuAccum::new(1);
        accum.advance(1.0, &[0.0], 100);
        let lines = accum.to_cpu_lines();
        assert_eq!(lines[1].fields[3], 100);
        assert_eq!(lines[1].fields[4], 0);
        assert_eq!(lines[1].idle(), 100);
    }

    #[test]
    fn cpu_accum_resizes_when_core_count_changes() {
        let mut accum = CpuAccum::new(2);
        accum.advance(1.0, &[50.0, 50.0], 100);
        accum.advance(1.0, &[100.0, 100.0, 100.0], 100);
        let lines = accum.to_cpu_lines();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[3].name, "cpu2");
        // Surviving cores keep their history; the new core is zero-filled and
        // only carries the second advance.
        assert_eq!(lines[1].fields, vec![150, 0, 0, 50, 0]);
        assert_eq!(lines[3].fields, vec![100, 0, 0, 0, 0]);
    }

    #[test]
    fn starttime_ticks_round_trip() {
        let st = starttime_ticks(10_000.0, 3_600, CLK_TZ);
        assert_eq!(st, 640_000);
        // detect.rs's elapsed formula: (uptime - starttime/hz).max(0)
        let elapsed = (10_000.0_f64 - st as f64 / CLK_TZ as f64).max(0.0) as u64;
        assert_eq!(elapsed, 3_600);
    }

    #[test]
    fn to_proc_entry_units_and_passthrough() {
        let raw = RawProc {
            pid: 42,
            ppid: 7,
            name: "claude".to_string(),
            accumulated_cpu_ms: 1_234,
            mem_bytes: 8_388_608,
            run_secs: 61,
            cmdline: vec![
                "claude".to_string(),
                "--model".to_string(),
                "glm-5.2".to_string(),
            ],
            cwd: Some(std::path::PathBuf::from("/Users/me/proj")),
        };
        let e = to_proc_entry(raw, 1_000.0, CLK_TZ);
        assert_eq!(e.stat.pid, 42);
        assert_eq!(e.stat.ppid, 7);
        assert_eq!(e.stat.comm, "claude");
        assert_eq!(e.stat.utime, 123, "ms/10");
        assert_eq!(e.stat.stime, 0);
        assert_eq!(e.stat.rss_pages, 2_048, "bytes/4096");
        assert_eq!(
            e.stat.rss_kib() as u64,
            8_388_608 / 1024,
            "rss_kib == bytes/1024"
        );
        assert_eq!(e.stat.starttime, 93_900);
        assert_eq!(e.cmdline, vec!["claude", "--model", "glm-5.2"]);
        assert_eq!(
            e.cwd.as_deref(),
            Some(std::path::Path::new("/Users/me/proj"))
        );
    }

    #[test]
    fn end_to_end_build_sessions_off_mac() {
        let hz = CLK_TZ;
        let mut accum = CpuAccum::new(2);
        let raw = |cpu_ms: u64| RawProc {
            pid: 1,
            ppid: 0,
            name: "claude".to_string(),
            accumulated_cpu_ms: cpu_ms,
            mem_bytes: 4_194_304,
            run_secs: 100,
            cmdline: vec!["claude".to_string()],
            cwd: None,
        };
        let mut snap0 = Snapshot {
            cpus: accum.to_cpu_lines(),
            uptime: Uptime { secs: 10_000.0 },
            tick_hz: hz,
            ..Default::default()
        };
        snap0.procs.insert(1, to_proc_entry(raw(0), 10_000.0, hz));

        accum.advance(2.0, &[25.0, 75.0], hz);
        let mut snap1 = Snapshot {
            cpus: accum.to_cpu_lines(),
            uptime: Uptime { secs: 10_002.0 },
            tick_hz: hz,
            ..Default::default()
        };
        snap1
            .procs
            .insert(1, to_proc_entry(raw(1_000), 10_002.0, hz));

        let sessions = build_sessions(&snap1, Some(&snap0));
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.kind.id, "claude");
        // Δproc = 1000ms/10 = 100 ticks; Δtotal = 2.0*100*2 = 400 ticks;
        // 100/400 * 100 * 2 cores = 50%.
        assert!((s.cpu_percent - 50.0).abs() < 0.1, "got {}", s.cpu_percent);
        assert_eq!(s.rss_kib, 4_096);
    }
}
