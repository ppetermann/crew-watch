//! Application state owned by the main loop.

use std::time::Duration;

use crate::detect::{build_sessions, Session};
use crate::meta::TaskRecord;
use crate::model::resolve_model;
use crate::procfs::{collect, Snapshot};
use crate::taskinfo::TaskInfo;
use crate::titles::TaskTitles;

pub struct App {
    pub interval: Duration,
    pub records: Vec<TaskRecord>,
    pub titles: TaskTitles,
    pub curr: Option<Snapshot>,
    pub prev: Option<Snapshot>,
    pub sessions: Vec<Session>,
}

impl App {
    pub fn new(interval: Duration, records: Vec<TaskRecord>, titles: TaskTitles) -> Self {
        Self {
            interval,
            records,
            titles,
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

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.curr.as_ref()
    }
}
