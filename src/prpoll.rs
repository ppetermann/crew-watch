//! Firstmate PR merge-poll sidecar parsing.
//!
//! When firstmate arms a merge watch for a task's pull request it writes
//! `state/<stem>.pr-poll`: exactly five newline-terminated lines — provider,
//! url, host, path, number — which firstmate's own `bin/fm-pr-poll.sh` reads
//! back with one `read -r` per line, refusing a symlink, a short file, a sixth
//! line, or a number that is not a positive integer without a leading zero.
//! This parser applies that framing and number rule, then a URL-shape check
//! from the same script: the provider must be one firstmate polls, host and
//! path must be plain names, and the URL must be exactly the one rebuilt from
//! them, so a doctored sidecar cannot hand a caller an arbitrary link. It does
//! not repeat the rest of firstmate's arm-time validation (name length limits,
//! GitLab's reserved path segments, the registration record), so a sidecar
//! that passes here is well-shaped, not proven armed.
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
    let [provider, url, host, path, number] = fields[..] else {
        return None;
    };
    if !number.starts_with(|c: char| ('1'..='9').contains(&c))
        || !number.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    if url != expected_url(provider, host, path, number)? {
        return None;
    }
    Some(PrPoll {
        provider: provider.to_string(),
        url: url.to_string(),
        number: number.parse().ok()?,
    })
}

/// The change URL `fm-pr-poll.sh` rebuilds from a sidecar's provider, host,
/// path and number, or `None` when those components fail its shape rules.
fn expected_url(provider: &str, host: &str, path: &str, number: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('/').collect();
    if !segments.iter().all(|s| is_path_segment(s)) {
        return None; // also rejects a leading, trailing or doubled `/`
    }
    if provider == "github" {
        return (host == "github.com" && segments.len() == 2)
            .then(|| format!("https://github.com/{path}/pull/{number}"));
    }
    if host == "github.com" || !is_host(host) {
        return None;
    }
    match provider {
        "gitlab" if segments.len() >= 2 => {
            Some(format!("https://{host}/{path}/-/merge_requests/{number}"))
        }
        "gerrit" => Some(format!("https://{host}/c/{path}/+/{number}")),
        "forgejo" if segments.len() == 2 => Some(format!("https://{host}/{path}/pulls/{number}")),
        _ => None,
    }
}

/// A lowercase DNS name: `[a-z0-9.-]`, no leading, trailing or doubled dot.
fn is_host(host: &str) -> bool {
    (1..=253).contains(&host.len())
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..")
}

/// One owner, group or repository name: `[A-Za-z0-9._-]+`, not `.` or `..`.
fn is_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
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
    fn each_provider_needs_its_rebuilt_url() {
        let parse = |provider: &str, url: &str, host: &str, path: &str| {
            parse_pr_poll(&format!("{provider}\n{url}\n{host}\n{path}\n7\n"))
        };
        for (provider, url, host, path) in [
            (
                "gitlab",
                "https://gitlab.example.org/group/sub/proj/-/merge_requests/7",
                "gitlab.example.org",
                "group/sub/proj",
            ),
            (
                "gerrit",
                "https://review.example.org/c/project/+/7",
                "review.example.org",
                "project",
            ),
            (
                "forgejo",
                "https://code.example.org/owner/repo/pulls/7",
                "code.example.org",
                "owner/repo",
            ),
        ] {
            let pr = parse(provider, url, host, path).expect(provider);
            assert_eq!((pr.provider.as_str(), pr.url.as_str()), (provider, url));
        }
    }

    #[test]
    fn a_doctored_sidecar_yields_no_link() {
        // Unknown provider with a script URL: firstmate's poll exits on it.
        assert_eq!(
            parse_pr_poll("bogus\njavascript:alert(1)\nevil.example\nx/y\n7\n"),
            None
        );
        let gh = |url: &str, host: &str, path: &str| {
            parse_pr_poll(&format!("github\n{url}\n{host}\n{path}\n48\n"))
        };
        // The URL must be the one rebuilt from host, path and number.
        assert_eq!(gh("https://evil.example/x", "github.com", "o/r"), None);
        assert_eq!(
            gh("https://github.com/o/r/pull/49", "github.com", "o/r"),
            None
        );
        // GitHub means github.com and owner/repo; names are plain.
        assert_eq!(
            gh("https://evil.example/o/r/pull/48", "evil.example", "o/r"),
            None
        );
        assert_eq!(
            gh("https://github.com/o/r/x/pull/48", "github.com", "o/r/x"),
            None
        );
        assert_eq!(
            gh("https://github.com/o/../pull/48", "github.com", "o/.."),
            None
        );
        // Other providers never point at github.com or a malformed host.
        for host in ["github.com", "Evil.example", ".evil", "evil..example", ""] {
            let url = format!("https://{host}/o/r/pulls/7");
            assert_eq!(
                parse_pr_poll(&format!("forgejo\n{url}\n{host}\no/r\n7\n")),
                None,
                "{host:?}"
            );
        }
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
