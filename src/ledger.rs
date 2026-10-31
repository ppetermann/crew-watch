//! firstmate's fleet activity ledger, `state/fleet-ledger.jsonl`.
//!
//! The ledger is firstmate's documented hook for outside readers
//! (`docs/fleet-ledger.md` in firstmate is its contract): an opt-in,
//! append-only JSON Lines file, one record per line, each with `v`, `ts`,
//! `event` and `task`, and per event a few more members (`task.dispatched`:
//! `kind`, `project`, `harness`, `model`; `task.status`: `state`, `key`,
//! `text`; `task.pr_ready`: `pr`; `task.merged`: `via` and `pr`;
//! `task.cleaned_up`: none). Readers ignore members and events they do not
//! know, records may repeat (delivery is at least once), and the file is
//! never rotated, only truncated by hand.
//!
//! Unlike the per-task files, the ledger outlives the task, so it is how a
//! reader sees work that has landed and been cleaned up. It grows without
//! bound, so [`LedgerTail`] reads it incrementally: each call returns only
//! the records appended since the last.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One ledger record. Members an event does not carry are `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerRecord {
    /// Record format version (`1`).
    pub v: u64,
    /// Unix time the record was written.
    pub ts: u64,
    /// `task.dispatched`, `task.status`, `task.pr_ready`, `task.merged`,
    /// `task.cleaned_up`, or a later event this reader does not know.
    pub event: String,
    pub task: String,
    /// `task.dispatched`: `ship`, `scout` or `secondmate`.
    #[serde(default)]
    pub kind: Option<String>,
    /// `task.dispatched`: the project directory name.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// `task.status`: the status line's verb, its decision key and its text.
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    /// `task.pr_ready`, and `task.merged` via a PR: the PR's URL.
    #[serde(default)]
    pub pr: Option<String>,
    /// `task.merged`: `pr` or `local`.
    #[serde(default)]
    pub via: Option<String>,
}

/// One ledger line as a record; `None` for a blank or malformed line, or one
/// missing `v`, `ts`, `event` or `task`.
pub fn parse_ledger_line(line: &str) -> Option<LedgerRecord> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// The ledger's path under a firstmate home.
pub fn ledger_path(fm_home: &Path) -> PathBuf {
    fm_home.join("state").join("fleet-ledger.jsonl")
}

/// Reads a ledger file from where the last read stopped.
///
/// The first [`LedgerTail::read_new`] returns every record in the file; each
/// later one only what was appended since. A line still being written (no
/// newline yet) waits for the next read. A file that got shorter was
/// truncated, so reading starts again from its beginning; a file that is
/// missing reads as empty and is picked up once it appears.
#[derive(Debug)]
pub struct LedgerTail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl LedgerTail {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            partial: Vec::new(),
        }
    }

    /// A tail of `state/fleet-ledger.jsonl` under `fm_home`.
    pub fn for_home(fm_home: &Path) -> Self {
        Self::new(ledger_path(fm_home))
    }

    /// The records appended since the last call, in file order. Never fails:
    /// an unreadable file returns nothing and is tried again next time.
    pub fn read_new(&mut self) -> Vec<LedgerRecord> {
        let Ok(mut file) = File::open(&self.path) else {
            return Vec::new();
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return Vec::new();
        };
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut fresh = Vec::new();
        let Ok(read) = file.take(len - self.offset).read_to_end(&mut fresh) else {
            return Vec::new();
        };
        self.offset += read as u64;
        self.partial.extend_from_slice(&fresh);
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return Vec::new();
        };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        String::from_utf8_lossy(&complete)
            .lines()
            .filter_map(parse_ledger_line)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    fn temp_file(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "crew-watch-ledger-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("fleet-ledger.jsonl")
    }

    fn append(path: &Path, text: &str) {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    // The contract's own example.
    const EXAMPLE: &str = r#"{"v":1,"ts":1790132857,"event":"task.dispatched","task":"fix-login","kind":"ship","project":"webapp","harness":"claude","model":null}
{"v":1,"ts":1790132870,"event":"task.status","task":"fix-login","state":"working","key":null,"text":" bug reproduced"}
{"v":1,"ts":1790133400,"event":"task.status","task":"fix-login","state":"done","key":null,"text":" PR https://github.com/acme/webapp/pull/7 checks green"}
{"v":1,"ts":1790133410,"event":"task.pr_ready","task":"fix-login","pr":"https://github.com/acme/webapp/pull/7"}
{"v":1,"ts":1790133900,"event":"task.merged","task":"fix-login","via":"pr","pr":"https://github.com/acme/webapp/pull/7"}
{"v":1,"ts":1790133960,"event":"task.cleaned_up","task":"fix-login"}
"#;

    #[test]
    fn parses_every_event_of_the_contract() {
        let records: Vec<LedgerRecord> = EXAMPLE.lines().filter_map(parse_ledger_line).collect();
        assert_eq!(records.len(), 6);
        assert_eq!(records[0].kind.as_deref(), Some("ship"));
        assert_eq!(records[0].project.as_deref(), Some("webapp"));
        assert_eq!(records[0].model, None);
        assert_eq!(records[1].state.as_deref(), Some("working"));
        assert_eq!(records[1].text.as_deref(), Some(" bug reproduced"));
        assert_eq!(records[3].event, "task.pr_ready");
        assert_eq!(
            records[4],
            LedgerRecord {
                v: 1,
                ts: 1_790_133_900,
                event: "task.merged".into(),
                task: "fix-login".into(),
                via: Some("pr".into()),
                pr: Some("https://github.com/acme/webapp/pull/7".into()),
                ..LedgerRecord::default()
            }
        );
        assert_eq!(records[5].event, "task.cleaned_up");
    }

    #[test]
    fn ignores_unknown_members_and_skips_what_is_not_a_record() {
        let later = r#"{"v":2,"ts":5,"event":"session.started","task":"t","new":{"x":1}}"#;
        assert_eq!(parse_ledger_line(later).unwrap().event, "session.started");
        for bad in [
            "",
            "   ",
            "not json",
            r#"{"v":1,"ts":5,"event":"task.status"}"#,
            r#"{"v":1,"event":"task.status","task":"t"}"#,
            r#"{"v":1,"ts":"5","event":"task.status","task":"t"}"#,
            r#"{"v":1,"ts":5,"event":"task.status","task":"t""#,
        ] {
            assert_eq!(parse_ledger_line(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn tail_returns_only_what_was_appended() {
        let path = temp_file("append");
        let mut tail = LedgerTail::new(path.clone());
        assert!(tail.read_new().is_empty(), "a missing file reads empty");
        append(&path, EXAMPLE);
        assert_eq!(tail.read_new().len(), 6);
        assert!(tail.read_new().is_empty());
        // A line still being written waits for its newline.
        append(&path, r#"{"v":1,"ts":7,"event":"task.dispatched","#);
        assert!(tail.read_new().is_empty());
        append(&path, "\"task\":\"next\",\"kind\":\"scout\"}\ngarbage\n");
        let fresh = tail.read_new();
        assert_eq!(fresh.len(), 1);
        assert_eq!((fresh[0].task.as_str(), fresh[0].ts), ("next", 7));
    }

    #[test]
    fn tail_starts_over_after_a_truncation() {
        let path = temp_file("truncate");
        let mut tail = LedgerTail::new(path.clone());
        append(&path, EXAMPLE);
        assert_eq!(tail.read_new().len(), 6);
        fs::write(&path, "").unwrap();
        assert!(tail.read_new().is_empty());
        append(
            &path,
            r#"{"v":1,"ts":9,"event":"task.cleaned_up","task":"a"}"#,
        );
        append(&path, "\n");
        let fresh = tail.read_new();
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].task, "a");
        assert_eq!(
            LedgerTail::for_home(Path::new("/h")).path,
            Path::new("/h/state/fleet-ledger.jsonl")
        );
    }
}
