//! crew-watch configuration file.
//!
//! crew-watch had no config file before the quota row (flags + env only). A TOML
//! dep for one list is overkill, so this mirrors the `key=value` idiom of
//! [`crate::meta`] (firstmate `*.meta` records) and is tested the same way.
//!
//! Path: `${XDG_CONFIG_HOME:-$HOME/.config}/crew-watch/config`. No `$HOME` ⇒
//! persistence is disabled for the session (the in-memory selection still
//! works). Unknown keys are preserved verbatim on save so future keys are
//! forward-compatible.

use std::fs;
use std::path::{Path, PathBuf};

/// Parsed configuration. Only the keys crew-watch knows today; everything else
/// is carried through untouched on save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// `None` (key/file absent) ⇒ auto mode: show every provider with ≥1
    /// window. `Some([])` (explicit `quota_providers=`) ⇒ row hidden until
    /// re-enabled via the dialog. `Some([ids])` ⇒ only those providers, in
    /// report order at render time.
    pub quota_providers: Option<Vec<String>>,
}

/// Resolve the config file path, or `None` if neither `XDG_CONFIG_HOME` nor
/// `$HOME` is set.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("crew-watch").join("config"))
}

/// Parse a config file. Missing/unreadable ⇒ defaults; never errors.
pub fn load(path: &Path) -> Config {
    let Ok(content) = fs::read_to_string(path) else {
        return Config::default();
    };
    parse(&content)
}

/// Parse config text (pure; testable without touching the filesystem).
pub fn parse(content: &str) -> Config {
    let mut cfg = Config::default();
    for line in content.lines() {
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "quota_providers" {
            cfg.quota_providers = Some(
                val.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        }
    }
    cfg
}

/// Save the `quota_providers` selection, rewriting the file: every existing
/// line except an old `quota_providers` is preserved verbatim, and the new
/// `quota_providers=` line is written (or replaced). Creates parent dirs.
/// Returns `Err(message)` on I/O failure; never panics.
pub fn save_quota_providers(path: &Path, ids: &[String]) -> Result<(), String> {
    let mut out = String::new();
    // Preserve any pre-existing unknown keys, dropping the old selection line.
    if let Ok(existing) = fs::read_to_string(path) {
        for line in existing.lines() {
            let is_quota = line
                .split_once('=')
                .map(|(k, _)| k.trim() == "quota_providers")
                .unwrap_or(false);
            if !is_quota {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    // Header only on a fresh file.
    if out.is_empty() {
        out.push_str("# crew-watch configuration\n");
    }
    out.push_str("quota_providers=");
    out.push_str(&ids.join(","));
    out.push('\n');

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    }
    fs::write(path, out).map_err(|e| format!("write config: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Unique temp dir under /tmp, mirroring the helpers in app.rs / titles.rs.
    fn tempfile_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "crew-watch-cfg-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_is_auto_mode() {
        let cfg = load(Path::new("/nonexistent/crew-watch-cfg-99999/config"));
        assert_eq!(cfg.quota_providers, None);
    }

    #[test]
    fn parse_key_absent_is_none() {
        let cfg = parse("# comment\nother_key=val\n");
        assert_eq!(cfg.quota_providers, None);
    }

    #[test]
    fn parse_empty_value_is_explicit_empty() {
        let cfg = parse("quota_providers=\n");
        assert_eq!(cfg.quota_providers, Some(Vec::new()));
    }

    #[test]
    fn parse_single_and_multi() {
        assert_eq!(
            parse("quota_providers=claude\n").quota_providers,
            Some(vec!["claude".to_string()])
        );
        assert_eq!(
            parse("quota_providers=claude, codex ,grok\n").quota_providers,
            Some(vec![
                "claude".to_string(),
                "codex".to_string(),
                "grok".to_string()
            ])
        );
    }

    #[test]
    fn parse_trailing_comma_drops_empties() {
        assert_eq!(
            parse("quota_providers=claude,\n").quota_providers,
            Some(vec!["claude".to_string()])
        );
    }

    #[test]
    fn round_trip_preserves_selection() {
        let dir = tempfile_dir("roundtrip");
        let path = dir.join("config");
        save_quota_providers(&path, &["claude".into(), "codex".into()]).unwrap();
        let loaded = load(&path);
        assert_eq!(
            loaded.quota_providers,
            Some(vec!["claude".to_string(), "codex".to_string()])
        );
    }

    #[test]
    fn save_unknown_keys_preserved() {
        let dir = tempfile_dir("preserve");
        let path = dir.join("config");
        fs::write(&path, "# header\nfuture_key=42\nquota_providers=claude\n").unwrap();
        save_quota_providers(&path, &["codex".into()]).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("future_key=42"),
            "unknown key kept: {content}"
        );
        assert!(
            content.contains("quota_providers=codex")
                && !content.contains("quota_providers=claude"),
            "old selection replaced: {content}"
        );
    }

    #[test]
    fn save_empty_writes_explicit_empty() {
        let dir = tempfile_dir("empty");
        let path = dir.join("config");
        save_quota_providers(&path, &[]).unwrap();
        assert_eq!(load(&path).quota_providers, Some(Vec::new()));
    }

    #[test]
    fn save_unwritable_returns_err_no_panic() {
        // A path whose parent does not exist AND cannot be created (a file
        // occupies a parent component) must error, not panic.
        let dir = tempfile_dir("unwritable");
        let blocker = dir.join("blocker");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("config"); // blocker is a file, not a dir
        let res = save_quota_providers(&path, &["claude".into()]);
        assert!(res.is_err(), "expected err, got {res:?}");
    }
}
