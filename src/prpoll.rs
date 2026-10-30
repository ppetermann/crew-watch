//! Firstmate PR merge-poll sidecar parsing.
//!
//! When firstmate arms a merge watch for a task's pull request it writes
//! `state/<stem>.pr-poll`: exactly five newline-terminated lines — provider,
//! url, host, path, number — which firstmate's own `bin/fm-pr-poll.sh` reads
//! back with one `read -r` per line, refusing a symlink, a short file, a sixth
//! line, or a number that is not a positive integer without a leading zero.
//! This parser applies the same rules, so a row shows a PR exactly when
//! firstmate's poll would follow it.
//!
//! Read-only and best-effort like [`crate::meta`]: anything missing or
//! malformed is `None`, never an error.

use std::fs;
use std::path::Path;

use serde::Serialize;

/// The pull request a task's merge poll is armed for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrPoll {
    /// Forge provider tag as firstmate records it (`github`, `gitlab`, ...).
    pub provider: String,
    pub url: String,
    pub number: u64,
}

/// Parse the contents of a `.pr-poll` sidecar.
pub fn parse_pr_poll(content: &str) -> Option<PrPoll> {
    let mut lines = content.split_inclusive('\n');
    let mut fields = Vec::with_capacity(5);
    for _ in 0..5 {
        // `read -r` fails on a final line without its newline, so does this.
        fields.push(lines.next()?.strip_suffix('\n')?);
    }
    // A sixth complete line makes the sidecar invalid; trailing bytes without
    // a newline are what `read -r` would also skip past.
    if lines.next().is_some_and(|l| l.ends_with('\n')) {
        return None;
    }
    let number = fields[4];
    if !number.starts_with(|c: char| ('1'..='9').contains(&c))
        || !number.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some(PrPoll {
        provider: fields[0].to_string(),
        url: fields[1].to_string(),
        number: number.parse().ok()?,
    })
}

/// Load `state/<stem>.pr-poll` under a firstmate home. A symlink is refused,
/// as firstmate's poll refuses it.
pub fn load_pr_poll(fm_home: &Path, stem: &str) -> Option<PrPoll> {
    let path = fm_home.join("state").join(format!("{stem}.pr-poll"));
    if !fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    parse_pr_poll(&fs::read_to_string(path).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://github.com/ppetermann/crew-watch/pull/48";

    fn sidecar(number: &str) -> String {
        format!("github\n{URL}\ngithub.com\nppetermann/crew-watch\n{number}\n")
    }

    #[test]
    fn five_lines_parse() {
        assert_eq!(
            parse_pr_poll(&sidecar("48")),
            Some(PrPoll {
                provider: "github".to_string(),
                url: URL.to_string(),
                number: 48,
            })
        );
    }

    #[test]
    fn short_or_unterminated_is_none() {
        assert_eq!(parse_pr_poll(""), None);
        assert_eq!(parse_pr_poll("github\nurl\nhost\npath\n"), None);
        // The fifth line must end in a newline, as `read -r` requires.
        assert_eq!(parse_pr_poll("github\nurl\nhost\npath\n48"), None);
    }

    #[test]
    fn a_sixth_line_is_none_but_trailing_bytes_are_ignored() {
        assert_eq!(parse_pr_poll(&format!("{}extra\n", sidecar("48"))), None);
        assert_eq!(parse_pr_poll(&format!("{}\n", sidecar("48"))), None);
        assert!(parse_pr_poll(&format!("{}junk", sidecar("48"))).is_some());
    }

    #[test]
    fn number_must_be_a_positive_integer_without_leading_zero() {
        for bad in ["0", "048", "", "4a", "-4", " 48", "48 "] {
            assert_eq!(parse_pr_poll(&sidecar(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn load_reads_the_stem_sidecar_and_refuses_a_symlink() {
        let home = std::env::temp_dir().join(format!(
            "crew-watch-prpoll-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let state = home.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("task.pr-poll"), sidecar("48")).unwrap();
        assert_eq!(load_pr_poll(&home, "task").map(|p| p.number), Some(48));
        assert_eq!(load_pr_poll(&home, "missing"), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(state.join("task.pr-poll"), state.join("link.pr-poll"))
                .unwrap();
            assert_eq!(load_pr_poll(&home, "link"), None);
        }
        let _ = fs::remove_dir_all(&home);
    }
}
