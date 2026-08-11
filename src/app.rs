//! Application state owned by the main loop.

use std::time::Duration;

use crate::detect::{build_sessions, Session};
use crate::meta::TaskRecord;
use crate::procfs::{collect, Snapshot};
use crate::taskinfo::TaskInfo;

pub struct App {
    pub interval: Duration,
    pub records: Vec<TaskRecord>,
    pub curr: Option<Snapshot>,
    pub prev: Option<Snapshot>,
    pub sessions: Vec<Session>,
}

impl App {
    pub fn new(interval: Duration, records: Vec<TaskRecord>) -> Self {
        Self {
            interval,
            records,
            curr: None,
            prev: None,
            sessions: Vec::new(),
        }
    }

    /// Collect one /proc snapshot, rebuild sessions, and resolve task info.
    pub fn tick(&mut self) {
        let snap = collect();
        let mut sessions = build_sessions(&snap, self.curr.as_ref());
        let records = &self.records;
        for s in &mut sessions {
            if let Some(entry) = snap.procs.get(&s.pid) {
                s.task = TaskInfo::resolve(records, entry.cwd.as_deref(), &entry.cmdline, s.kind);
            }
        }
        self.prev = self.curr.take();
        self.curr = Some(snap);
        self.sessions = sessions;
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.curr.as_ref()
    }
}
