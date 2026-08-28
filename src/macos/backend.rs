//! macOS snapshot collector on the `sysinfo` crate: fills the same
//! [`Snapshot`] contract as the Linux `/proc` reader.
//!
//! Known edge cases (verify on a real Mac):
//! - sysinfo ignores CPU-usage refreshes closer than its
//!   `MINIMUM_CPU_UPDATE_INTERVAL` (200ms), so at `--interval` rates below
//!   that the macOS meters update every other tick.
//! - Other users' processes have unreadable argv/cwd without root
//!   (`KERN_PROCARGS2`/`proc_pidinfo` restrictions): degraded rows, no
//!   fleet TASK/STATE. Own agents are unaffected.
//! - macOS swap is dynamically sized, so the swap meter's denominator moves.
//! - `available_memory()` approximates Linux `MemAvailable`
//!   (free + inactive/purgeable).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

use crate::macos::convert::{to_proc_entry, CpuAccum, RawProc};
use crate::procfs::{LoadAvg, MemInfo, Snapshot, Uptime, CLK_TZ};

static STATE: OnceLock<Mutex<Backend>> = OnceLock::new();

struct Backend {
    sys: System,
    accum: CpuAccum,
    last: Instant,
}

/// Collect one system snapshot via sysinfo. Only ever runs on the tick
/// thread (the quota poller runs off-path by contract), so the backend state
/// is a plain mutex and a poisoned lock cannot occur in practice.
pub fn collect() -> Snapshot {
    let state = STATE.get_or_init(|| {
        Mutex::new(Backend {
            sys: System::new(),
            accum: CpuAccum::new(0),
            last: Instant::now(),
        })
    });
    let mut backend = state.lock().unwrap();
    let elapsed = backend.last.elapsed().as_secs_f64();
    backend.last = Instant::now();

    backend.sys.refresh_cpu_usage();
    backend.sys.refresh_memory();
    backend.sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );

    let usage: Vec<f32> = backend
        .sys
        .cpus()
        .iter()
        .map(|cpu| cpu.cpu_usage())
        .collect();
    backend.accum.advance(elapsed, &usage, CLK_TZ);
    let cpus = backend.accum.to_cpu_lines();

    let uptime_secs = System::uptime() as f64;

    let mut procs = HashMap::new();
    for (pid, process) in backend.sys.processes() {
        let raw = RawProc {
            pid: pid.as_u32() as i32,
            ppid: process.parent().map(|p| p.as_u32() as i32).unwrap_or(0),
            name: process.name().to_string_lossy().into_owned(),
            accumulated_cpu_ms: process.accumulated_cpu_time(),
            mem_bytes: process.memory(),
            run_secs: process.run_time(),
            cmdline: process
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect(),
            cwd: process.cwd().map(|p| p.to_path_buf()),
        };
        let entry = to_proc_entry(raw, uptime_secs, CLK_TZ);
        procs.insert(entry.stat.pid, entry);
    }

    let mem = MemInfo {
        mem_total_kib: backend.sys.total_memory() / 1024,
        mem_avail_kib: backend.sys.available_memory() / 1024,
        swap_total_kib: backend.sys.total_swap() / 1024,
        swap_free_kib: backend.sys.free_swap() / 1024,
    };

    let la = System::load_average();
    let running = backend
        .sys
        .processes()
        .values()
        .filter(|p| p.status() == ProcessStatus::Run)
        .count() as u64;
    let load = LoadAvg {
        one: la.one,
        five: la.five,
        fifteen: la.fifteen,
        running,
        total: procs.len() as u64,
    };

    Snapshot {
        procs,
        cpus,
        mem,
        load,
        uptime: Uptime { secs: uptime_secs },
        tick_hz: CLK_TZ,
    }
}
