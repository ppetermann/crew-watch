//! Resolve a human project name from a process's cwd.
//!
//! The cwd basename is a good project name when the agent is running inside a
//! project checkout (e.g. cwd `/home/crew/firstmate` -> `firstmate`). But some
//! fleet tooling (no-mistakes worktrees) puts the agent in a ULID-named
//! directory inside a bare-repo worktree, where the basename is opaque. In that
//! case we walk the git worktree pointer (`<dir>/.git`) to the bare repo's
//! config and read the `origin` remote URL to recover the real repo name.
//!
//! All filesystem access is best-effort and non-fatal: any read or parse
//! failure degrades silently to the cwd basename.

use std::fs;
use std::path::{Path, PathBuf};

/// Resolve the project name for a working directory.
///
/// 1. If the cwd is inside a git repo (a `.git` file or dir is found by walking
///    up), return the repo name: for a normal repo, the directory containing
///    `.git/`; for a worktree (`.git` file pointing at a bare repo), the repo
///    name parsed from the bare repo's `origin` remote URL.
/// 2. Otherwise, return the cwd basename.
///
/// Returns `None` only when the cwd itself has no basename (root edge case).
pub fn resolve_project_name(cwd: &Path) -> Option<String> {
    for ancestor in cwd.ancestors() {
        let git = ancestor.join(".git");
        if let Ok(meta) = fs::metadata(&git) {
            if meta.is_dir() {
                return ancestor
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned());
            }
            // `.git` file: a worktree pointer (`gitdir: <path>`).
            if let Some(name) = resolve_worktree_repo_name(&git) {
                return Some(name);
            }
            // Pointer unreadable/unparseable: fall back to this dir's basename.
            return ancestor
                .file_name()
                .map(|s| s.to_string_lossy().into_owned());
        }
    }
    cwd.file_name().map(|s| s.to_string_lossy().into_owned())
}

/// Read a `.git` worktree-pointer file and resolve the repo name from the bare
/// repo's `origin` remote URL. Returns `None` on any I/O or parse failure.
fn resolve_worktree_repo_name(git_file: &Path) -> Option<String> {
    let content = fs::read_to_string(git_file).ok()?;
    let gitdir = parse_gitdir_pointer(&content)?;
    let config_path = repo_config_path_from_gitdir(Path::new(&gitdir))?;
    let config = fs::read_to_string(&config_path).ok()?;
    let url = parse_remote_origin_url(&config)?;
    Some(repo_name_from_url(&url))
}

/// Parse a git worktree pointer file: a single `gitdir: <path>` line.
pub fn parse_gitdir_pointer(content: &str) -> Option<String> {
    content.lines().find_map(|l| {
        l.trim()
            .strip_prefix("gitdir:")
            .map(|p| p.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// From a gitdir path inside a bare repo (typically
/// `<repo>.git/worktrees/<name>`), find the repo's `config` file by walking up
/// to the nearest ancestor directory whose name ends in `.git`.
pub fn repo_config_path_from_gitdir(gitdir: &Path) -> Option<PathBuf> {
    for dir in gitdir.ancestors() {
        let name = dir.file_name()?.to_str()?;
        if name.ends_with(".git") {
            return Some(dir.join("config"));
        }
    }
    None
}

/// Parse the `[remote "origin"]` section's `url` field from a git config file.
pub fn parse_remote_origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed.contains("\"origin\"");
            continue;
        }
        if in_origin {
            if let Some(rest) = trimmed.strip_prefix("url") {
                if let Some(val) = rest.trim_start().strip_prefix('=') {
                    let val = val.trim().to_string();
                    if !val.is_empty() {
                        return Some(val);
                    }
                }
            }
        }
    }
    None
}

/// Extract the repo name from a remote URL: the last path segment with any
/// trailing `.git` removed. Handles HTTPS and SCP-style SSH URLs.
pub fn repo_name_from_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let last = url.rsplit(['/', ':']).next().unwrap_or(url);
    last.trim_end_matches(".git").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_gitdir_pointer ---

    #[test]
    fn parse_gitdir_basic() {
        assert_eq!(
            parse_gitdir_pointer("gitdir: /repo/.git/worktrees/wt1"),
            Some("/repo/.git/worktrees/wt1".to_string())
        );
    }

    #[test]
    fn parse_gitdir_with_whitespace() {
        assert_eq!(
            parse_gitdir_pointer("  gitdir:   /path/to/gitdir  \n"),
            Some("/path/to/gitdir".to_string())
        );
    }

    #[test]
    fn parse_gitdir_empty_value_returns_none() {
        assert!(parse_gitdir_pointer("gitdir: \n").is_none());
        assert!(parse_gitdir_pointer("not a pointer").is_none());
    }

    // --- repo_config_path_from_gitdir ---

    #[test]
    fn config_path_from_standard_worktree_gitdir() {
        let gitdir = Path::new("/home/x/repo.git/worktrees/wt1");
        assert_eq!(
            repo_config_path_from_gitdir(gitdir),
            Some(PathBuf::from("/home/x/repo.git/config"))
        );
    }

    #[test]
    fn config_path_returns_none_without_git_ancestor() {
        assert!(repo_config_path_from_gitdir(Path::new("/home/x/repo/worktrees/wt1")).is_none());
    }

    // --- parse_remote_origin_url ---

    const SAMPLE_CONFIG: &str = "\
[core]
\trepositoryformatversion = 0
\tfilemode = true
[remote \"origin\"]
\turl = https://github.com/ppetermann/firstmate
\tfetch = +refs/heads/*:refs/remotes/origin/*
[branch \"main\"]
\tremote = origin
";

    #[test]
    fn parse_origin_url() {
        assert_eq!(
            parse_remote_origin_url(SAMPLE_CONFIG),
            Some("https://github.com/ppetermann/firstmate".to_string())
        );
    }

    #[test]
    fn parse_origin_url_missing_returns_none() {
        let config = "[core]\n\tfilemode = true\n";
        assert!(parse_remote_origin_url(config).is_none());
    }

    #[test]
    fn parse_origin_url_empty_value_returns_none() {
        let config = "[remote \"origin\"]\n\turl = \n";
        assert!(parse_remote_origin_url(config).is_none());
    }

    // --- repo_name_from_url ---

    #[test]
    fn name_from_https_url() {
        assert_eq!(
            repo_name_from_url("https://github.com/ppetermann/firstmate"),
            "firstmate"
        );
    }

    #[test]
    fn name_from_https_url_with_dot_git() {
        assert_eq!(
            repo_name_from_url("https://github.com/ppetermann/firstmate.git"),
            "firstmate"
        );
    }

    #[test]
    fn name_from_scp_style_ssh_url() {
        assert_eq!(
            repo_name_from_url("git@github.com:ppetermann/firstmate.git"),
            "firstmate"
        );
    }

    #[test]
    fn name_from_ssh_protocol_url() {
        assert_eq!(
            repo_name_from_url("ssh://git@github.com/ppetermann/crew-watch.git"),
            "crew-watch"
        );
    }

    #[test]
    fn name_strips_trailing_slash() {
        assert_eq!(repo_name_from_url("https://github.com/x/y/"), "y");
    }

    // --- resolve_project_name (integration) ---

    #[test]
    fn resolve_falls_back_to_basename_outside_git() {
        let cwd = Path::new("/tmp/some-random-dir-12345");
        // /tmp almost certainly has no .git in its ancestors.
        assert_eq!(
            resolve_project_name(cwd),
            Some("some-random-dir-12345".to_string())
        );
    }

    #[test]
    fn resolve_root_path_returns_none() {
        assert!(resolve_project_name(Path::new("/")).is_none());
    }
}
