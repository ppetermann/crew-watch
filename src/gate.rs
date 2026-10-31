//! A ship task's local-gate pass marker, `state/<stem>.gate-pass`.
//!
//! firstmate's `bin/fm-local-gate.sh` owns the file: on a green gate it writes
//! one line, `head=<sha> ts=<epoch>`, for the worktree's HEAD, and on a red
//! gate it removes the marker, so a marker never outlives a failed run. The
//! reader follows firstmate's own (`bin/fm-pr-check.sh`): the last `head=`
//! line, its first word, a full 40- or 64-digit lowercase hex object name
//! (`fm_pr_head_valid`), and a marker that is a symlink is not read. Read-only
//! and best-effort like [`crate::meta`].

use std::fs;
use std::path::Path;

use serde::Serialize;

/// A green local gate: the commit it passed on and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatePass {
    /// Full object name of the gated HEAD.
    pub head: String,
    /// Unix time the gate passed, when the marker's `ts=` is plain digits.
    pub ts: Option<u64>,
}

/// Parse a gate-pass marker's contents; `None` without a valid head.
pub fn parse_gate_pass(content: &str) -> Option<GatePass> {
    let line = content.lines().rev().find(|l| l.starts_with("head="))?;
    let mut words = line["head=".len()..].split(' ');
    let head = words.next()?;
    if !head_valid(head) {
        return None;
    }
    let ts = words
        .find_map(|w| w.strip_prefix("ts="))
        .filter(|v| !v.is_empty() && v.len() <= 12 && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok());
    Some(GatePass {
        head: head.to_string(),
        ts,
    })
}

/// Read `state/<stem>.gate-pass` under a firstmate home. Missing, a
/// symlink, unreadable or without a valid head: `None`.
pub fn load_gate_pass(fm_home: &Path, stem: &str) -> Option<GatePass> {
    let path = fm_home.join("state").join(format!("{stem}.gate-pass"));
    if fs::symlink_metadata(&path).ok()?.file_type().is_symlink() {
        return None;
    }
    parse_gate_pass(&fs::read_to_string(path).ok()?)
}

fn head_valid(head: &str) -> bool {
    matches!(head.len(), 40 | 64) && head.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "93d577ab0c7124172e1414f7d943252f9bc961f9";

    #[test]
    fn reads_the_head_and_its_time() {
        assert_eq!(
            parse_gate_pass(&format!("head={SHA} ts=1793439479\n")),
            Some(GatePass {
                head: SHA.to_string(),
                ts: Some(1_793_439_479),
            })
        );
        let sha256 = "a".repeat(64);
        assert_eq!(
            parse_gate_pass(&format!("head={sha256}\n")).map(|g| (g.head, g.ts)),
            Some((sha256, None))
        );
        // The last head= line counts, as firstmate's reader takes it.
        let two = format!("head={} ts=1\nhead={SHA} ts=2\n", "b".repeat(40));
        assert_eq!(parse_gate_pass(&two).unwrap().head, SHA);
    }

    #[test]
    fn rejects_what_firstmate_calls_no_valid_head() {
        for bad in [
            String::new(),
            "ts=5\n".to_string(),
            "head= ts=5\n".to_string(),
            "head=93d577a ts=5\n".to_string(),
            format!("head={} ts=5\n", SHA.to_uppercase()),
            format!(" head={SHA} ts=5\n"),
        ] {
            assert_eq!(parse_gate_pass(&bad), None, "{bad:?}");
        }
        // A malformed time keeps the head.
        assert_eq!(
            parse_gate_pass(&format!("head={SHA} ts=soon\n"))
                .unwrap()
                .ts,
            None
        );
    }

    #[test]
    fn loads_from_the_home_but_not_through_a_symlink() {
        let home = std::env::temp_dir().join(format!(
            "crew-watch-gate-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state = home.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("t.gate-pass"), format!("head={SHA} ts=7\n")).unwrap();
        assert_eq!(load_gate_pass(&home, "t").unwrap().ts, Some(7));
        assert_eq!(load_gate_pass(&home, "absent"), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(state.join("t.gate-pass"), state.join("l.gate-pass"))
                .unwrap();
            assert_eq!(load_gate_pass(&home, "l"), None);
        }
        let _ = fs::remove_dir_all(&home);
    }
}
