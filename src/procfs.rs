//! Pure parsers + a single-pass /proc snapshot collector.
//!
//! The parsing functions take `&str` (or `&[u8]`) and are unit-tested with
//! fixture data; [`collect`] does the filesystem reads and degrades gracefully
//! (no panic) when a `/proc` entry vanishes mid-read.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// `USER_HZ` / `SC_CLK_TCK` is 100 on every Linux on x86/arm64. Used to convert
/// `/proc/<pid>/stat` jiffies to seconds.
pub const CLK_TZ: u64 = 100;
/// Page size in bytes; 4096 on every Linux target crew-watch supports. Used to
/// convert the `rss` field (in pages) to KiB.
pub const PAGE_SIZE: u64 = 4096;

/// Per-process fields parsed from `/proc/<pid>/stat`.
#[derive(Debug, Clone, Default)]
pub struct ProcStat {
    pub pid: i32,
    pub ppid: i32,
    pub comm: String,
    pub utime: u64,
    pub stime: u64,
    pub starttime: u64,
    pub rss_pages: u64,
}

impl ProcStat {
    /// Resident set size in KiB.
    pub fn rss_kib(&self) -> u64 {
        self.rss_pages * (PAGE_SIZE / 1024)
    }
}

/// A single process, combining `/proc/<pid>/stat`, `/proc/<pid>/cmdline` and
/// `/proc/<pid>/cwd`.
#[derive(Debug, Clone, Default)]
pub struct ProcEntry {
    pub stat: ProcStat,
    pub cmdline: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// One `cpu` / `cpuN` line from `/proc/stat`.
#[derive(Debug, Clone)]
pub struct CpuLine {
    pub name: String,
    pub fields: Vec<u64>,
}

impl CpuLine {
    /// Total jiffies (sum of all fields).
    pub fn total(&self) -> u64 {
        self.fields.iter().sum()
    }
    /// Idle jiffies (idle + iowait, fields index 3 and 4).
    pub fn idle(&self) -> u64 {
        self.fields.get(3).copied().unwrap_or(0) + self.fields.get(4).copied().unwrap_or(0)
    }
    /// The aggregate `cpu` line (no trailing digit).
    pub fn is_aggregate(&self) -> bool {
        self.name == "cpu"
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemInfo {
    pub mem_total_kib: u64,
    pub mem_avail_kib: u64,
    pub swap_total_kib: u64,
    pub swap_free_kib: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LoadAvg {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
    pub running: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Uptime {
    pub secs: f64,
}

/// A complete, point-in-time view of the system. Built by a single pass over
/// `/proc`.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub procs: HashMap<i32, ProcEntry>,
    pub cpus: Vec<CpuLine>,
    pub mem: MemInfo,
    pub load: LoadAvg,
    pub uptime: Uptime,
    pub tick_hz: u64,
}

/// Parse `/proc/<pid>/stat`. `pid` is the directory pid (used as a fallback);
/// the pid encoded in the file is authoritative. Returns `None` if the line is
/// malformed (e.g. the process exited and the kernel recycled the entry).
pub fn parse_proc_pid_stat(pid: i32, content: &str) -> Option<ProcStat> {
    let open = content.find('(')?;
    let close = content.rfind(')')?;
    let comm = content[open + 1..close].to_string();
    let parsed_pid: i32 = content[..open].trim().parse().ok()?;
    let rest: Vec<&str> = content[close + 1..].split_whitespace().collect();
    let get = |i: usize| -> Option<u64> { rest.get(i).and_then(|s| s.parse().ok()) };
    // After the closing paren, token[0] is field 3 (state), token[k] is field k+3.
    Some(ProcStat {
        pid: parsed_pid,
        ppid: get_idx(&rest, 1).unwrap_or(0),
        comm,
        utime: get(11)?,
        stime: get(12).unwrap_or(0),
        starttime: get(19).unwrap_or(0),
        rss_pages: get(21).unwrap_or(0),
    })
    .map(|s| if s.pid == 0 { ProcStat { pid, ..s } } else { s })
}

fn get_idx(rest: &[&str], i: usize) -> Option<i32> {
    rest.get(i).and_then(|s| s.parse().ok())
}

/// Parse `/proc/<pid>/cmdline` (NUL-separated argv).
pub fn parse_cmdline(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Parse `/proc/stat`. Returns every `cpu*` line (aggregate first in kernel
/// output, but not assumed) plus `btime` if present.
pub fn parse_proc_stat(content: &str) -> (Vec<CpuLine>, Option<u64>) {
    let mut cpus = Vec::new();
    let mut btime = None;
    for line in content.lines() {
        let mut it = line.split_whitespace();
        let Some(first) = it.next() else {
            continue;
        };
        if first.starts_with("cpu") {
            let fields = it.filter_map(|t| t.parse::<u64>().ok()).collect::<Vec<_>>();
            cpus.push(CpuLine {
                name: first.to_string(),
                fields,
            });
        } else if first == "btime" {
            btime = it.next().and_then(|t| t.parse::<u64>().ok());
        }
    }
    (cpus, btime)
}

/// Parse `/proc/meminfo`.
pub fn parse_meminfo(content: &str) -> MemInfo {
    let mut m = MemInfo::default();
    for line in content.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let val = val.trim();
        let n = val
            .split_whitespace()
            .next()
            .and_then(|t| t.parse::<u64>().ok())
            .unwrap_or(0);
        match key.trim() {
            "MemTotal" => m.mem_total_kib = n,
            "MemAvailable" => m.mem_avail_kib = n,
            "SwapTotal" => m.swap_total_kib = n,
            "SwapFree" => m.swap_free_kib = n,
            _ => {}
        }
    }
    m
}

/// Parse `/proc/loadavg`: `1m 5m 15m running/total pid`.
pub fn parse_loadavg(content: &str) -> Option<LoadAvg> {
    let mut it = content.split_whitespace();
    let one = it.next()?.parse::<f64>().ok()?;
    let five = it.next()?.parse::<f64>().ok()?;
    let fifteen = it.next()?.parse::<f64>().ok()?;
    let rt = it.next()?;
    let (running, total) = rt
        .split_once('/')
        .and_then(|(r, t)| Some((r.parse().ok()?, t.parse().ok()?)))
        .unwrap_or((0, 0));
    Some(LoadAvg {
        one,
        five,
        fifteen,
        running,
        total,
    })
}

/// Parse `/proc/uptime` (first field, seconds since boot).
pub fn parse_uptime(content: &str) -> Option<Uptime> {
    let secs = content.split_whitespace().next()?.parse::<f64>().ok()?;
    Some(Uptime { secs })
}

/// Read `/proc` once and build a [`Snapshot`]. Never panics on missing /
/// vanishing entries: each per-pid file read is best-effort.
pub fn collect() -> Snapshot {
    let mut procs = HashMap::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                continue;
            };
            let Some(stat) = fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|s| parse_proc_pid_stat(pid, &s))
            else {
                continue;
            };
            let cmdline = fs::read(format!("/proc/{pid}/cmdline"))
                .map(|b| parse_cmdline(&b))
                .unwrap_or_default();
            let cwd = fs::read_link(format!("/proc/{pid}/cwd")).ok();
            procs.insert(pid, ProcEntry { stat, cmdline, cwd });
        }
    }

    let (cpus, _btime) = parse_proc_stat(&fs::read_to_string("/proc/stat").unwrap_or_default());
    let mem = parse_meminfo(&fs::read_to_string("/proc/meminfo").unwrap_or_default());
    let load =
        parse_loadavg(&fs::read_to_string("/proc/loadavg").unwrap_or_default()).unwrap_or_default();
    let uptime =
        parse_uptime(&fs::read_to_string("/proc/uptime").unwrap_or_default()).unwrap_or_default();

    Snapshot {
        procs,
        cpus,
        mem,
        load,
        uptime,
        tick_hz: CLK_TZ,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_basic() {
        // pid 1234, comm "my agent (x)", state R, ppid 10, with utime/stime/starttime/rss
        // placed at their real /proc/[pid]/stat field positions. After the comm,
        // token[k] is field k+3: utime(14)->tok[11], stime(15)->tok[12],
        // starttime(22)->tok[19], rss(24)->tok[21].
        let line = "1234 (my agent (x)) R 10 0 0 0 -1 4194304 0 0 0 0 5 7 0 0 20 0 1 0 99900 0 1 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        let s = parse_proc_pid_stat(1234, line).expect("parsed");
        assert_eq!(s.pid, 1234);
        assert_eq!(s.comm, "my agent (x)");
        assert_eq!(s.ppid, 10);
        assert_eq!(s.utime, 5);
        assert_eq!(s.stime, 7);
        assert_eq!(s.starttime, 99900);
        assert_eq!(s.rss_pages, 1);
    }

    #[test]
    fn stat_malformed_returns_none() {
        assert!(parse_proc_pid_stat(1, "garbage").is_none());
        assert!(parse_proc_pid_stat(1, "1 (no closing paren").is_none());
    }

    #[test]
    fn cmdline_nul_separated() {
        let bytes = b"opencode\0--model\0glm-5.2\0\0";
        let v = parse_cmdline(bytes);
        assert_eq!(v, vec!["opencode", "--model", "glm-5.2"]);
        assert!(parse_cmdline(b"").is_empty());
    }

    #[test]
    fn proc_stat_cpus_and_btime() {
        let content = "cpu  100 200 300 4000 50 10 5 0 0 0\n cpu0 10 20 30 2000 25 5 2 0 0 0\n cpu1 90 180 270 2000 25 5 3 0 0 0\n intr 12345\n btime 1786400000\n";
        let (cpus, btime) = parse_proc_stat(content);
        assert_eq!(cpus.len(), 3);
        assert!(cpus[0].is_aggregate());
        assert_eq!(cpus[0].total(), 4665);
        assert_eq!(cpus[0].idle(), 4000 + 50);
        assert!(!cpus[1].is_aggregate());
        assert_eq!(btime, Some(1_786_400_000));
    }

    #[test]
    fn meminfo_parse() {
        let content = "MemTotal:       16384000 kB\nMemAvailable:    8000000 kB\nSwapTotal:      2097152 kB\nSwapFree:       1048576 kB\n";
        let m = parse_meminfo(content);
        assert_eq!(m.mem_total_kib, 16_384_000);
        assert_eq!(m.mem_avail_kib, 8_000_000);
        assert_eq!(m.swap_total_kib, 2_097_152);
        assert_eq!(m.swap_free_kib, 1_048_576);
    }

    #[test]
    fn loadavg_parse() {
        let l = parse_loadavg("1.23 0.45 0.67 2/1234 5678\n").unwrap();
        assert!((l.one - 1.23).abs() < 1e-9);
        assert!((l.fifteen - 0.67).abs() < 1e-9);
        assert_eq!(l.running, 2);
        assert_eq!(l.total, 1234);
        assert!(parse_loadavg("bad").is_none());
    }

    #[test]
    fn uptime_parse() {
        let u = parse_uptime("12345.67 9999.0\n").unwrap();
        assert!((u.secs - 12345.67).abs() < 1e-6);
        assert!(parse_uptime("").is_none());
    }

    #[test]
    fn rss_kib_conversion() {
        let s = ProcStat {
            rss_pages: 1024,
            ..Default::default()
        };
        assert_eq!(s.rss_kib(), 4096); // 1024 pages * 4 KiB/page
    }
}
