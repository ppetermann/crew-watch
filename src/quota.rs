//! Quota data layer: parsing `quota-axi --json`, an ISO-8601 → epoch helper for
//! `resetsAt`, and the background poller.
//!
//! ## Cadence and isolation contract
//!
//! `quota-axi --json` is a Node subprocess that takes ~0.8s wall even when it
//! answers from its own cache, and can stall unboundedly when a provider fetch
//! really goes to the network. It is therefore **never** invoked on the 2s
//! `/proc` tick path: [`spawn_poller`] runs it on a background thread at its
//! own cadence (default 5 min, see the floor note on [`fetch_once`] / the
//! `--quota-interval` clamp in `cli.rs`), and each call is bounded by a 10s
//! kill-timeout. Results cross over an `mpsc` channel; the main loop drains it
//! non-blockingly. Every failure leg reduces to a [`QuotaFetch::Failed`], so a
//! missing/wedged quota-axi can never stall or kill the core monitor.
//!
//! ## Schema tolerance
//!
//! `quota-axi` reports schema 3 today. There is **no hard version gate** (D10):
//! parsing is best-effort against whatever fields are present — unknown fields
//! are ignored and optional ones default. A genuinely unparseable payload fails
//! with a message carrying the recovered `schemaVersion`, so an additive v4
//! still works and a broken one reports clearly.

use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Public model (clean, typed; built from the private serde structs below)
// ---------------------------------------------------------------------------

/// One complete `quota-axi --json` report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuotaReport {
    pub schema_version: Option<u64>,
    pub generated_at: String,
    pub providers: Vec<ProviderQuota>,
}

/// One provider's slice of the report.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderQuota {
    pub id: String,
    pub label: String,
    pub plan: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub status: ProviderStatus,
    /// `state.stale` — quota-axi's own per-provider freshness flag, independent
    /// of our fetch age.
    pub stale: bool,
    pub error: Option<String>,
}

/// A single usage window (e.g. `five_hour` / "session", `model:fable`).
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
    pub id: String,
    pub label: String,
    pub percent_used: f64,
    pub resets_at: Option<String>,
}

/// Per-provider `state.status`, parsed to the values quota-axi emits today; any
/// future value is preserved verbatim as [`ProviderStatus::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStatus {
    Fresh,
    Error,
    AuthRequired,
    Unknown(String),
}

// ---------------------------------------------------------------------------
// Private serde structs (camelCase, lenient; unknown fields ignored)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReport {
    #[serde(default)]
    schema_version: Option<u64>,
    #[serde(default)]
    generated_at: String,
    #[serde(default, rename = "providers")]
    providers: Vec<RawProvider>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProvider {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    windows: Vec<RawWindow>,
    #[serde(default)]
    state: RawState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWindow {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    percent_used: f64,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawState {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    stale: bool,
    #[serde(default)]
    error: Option<String>,
}

fn parse_status(s: &str) -> ProviderStatus {
    match s {
        "fresh" => ProviderStatus::Fresh,
        "error" => ProviderStatus::Error,
        "auth_required" => ProviderStatus::AuthRequired,
        other => ProviderStatus::Unknown(other.to_string()),
    }
}

/// Parse a `quota-axi --json` document. Lenient on optional/unknown fields;
/// returns `Err` carrying the recovered schema version only when the input is
/// not valid JSON or not a JSON object.
pub fn parse_report(input: &str) -> Result<QuotaReport, String> {
    let raw: RawReport = serde_json::from_str(input).map_err(|e| {
        // Recover the schema version (if any) for a clearer failure message.
        let schema = serde_json::from_str::<serde_json::Value>(input)
            .ok()
            .and_then(|v| v.get("schemaVersion").and_then(|s| s.as_u64()));
        match schema {
            Some(v) => format!("unrecognized output (schema v{v}): {e}"),
            None => format!("unrecognized output (no schema): {e}"),
        }
    })?;
    Ok(QuotaReport {
        schema_version: raw.schema_version,
        generated_at: raw.generated_at,
        providers: raw
            .providers
            .into_iter()
            .map(|p| ProviderQuota {
                id: p.provider,
                label: p.label,
                plan: p.plan,
                windows: p
                    .windows
                    .into_iter()
                    .map(|w| QuotaWindow {
                        id: w.id,
                        label: w.label,
                        percent_used: w.percent_used,
                        resets_at: w.resets_at,
                    })
                    .collect(),
                status: p
                    .state
                    .status
                    .as_deref()
                    .map(parse_status)
                    .unwrap_or(ProviderStatus::Unknown("missing".to_string())),
                stale: p.state.stale,
                error: p.state.error,
            })
            .collect(),
    })
}

/// True iff this provider carries at least one usage window to render bars for.
/// The single definition shared by the auto-selection seed (D7), the row, its
/// colouring, and `--once`, so they can never disagree.
///
/// Deliberately independent of [`ProviderStatus`]: quota-axi reports
/// `state.status = stale` whenever a live fetch failed and it served its cache,
/// which can happen at any moment. Freshness is a *display* signal (dim styling
/// / a `stale` suffix), never a visibility filter — hiding on it would blank the
/// row exactly when the last known reading matters most.
pub fn has_usage_windows(p: &ProviderQuota) -> bool {
    !p.windows.is_empty()
}

// ---------------------------------------------------------------------------
// ISO-8601 → UTC epoch seconds
// ---------------------------------------------------------------------------

/// Parse an ISO-8601 timestamp as seen in `quota-axi` output into UTC epoch
/// seconds. Handles `Z`, `±HH:MM` offsets, and optional fractional seconds;
/// returns `None` on anything else. Pure and deterministic so it can be
/// unit-tested without a system clock.
///
/// Implemented with Howard Hinnant's `days_from_civil` algorithm (no date
/// crate, matching the project's tested-pure-parser ethos).
pub fn parse_iso8601_utc_epoch(input: &str) -> Option<u64> {
    let b = input.as_bytes();
    // YYYY-MM-DDTHH:MM:SS  — 19 chars minimum for the local part.
    if b.len() < 19 {
        return None;
    }
    let two = |start: usize| -> Option<u64> {
        let s = std::str::from_utf8(b.get(start..start + 2)?).ok()?;
        s.parse::<u64>().ok()
    };
    if b[4] != b'-'
        || b[7] != b'-'
        || (b[10] != b'T' && b[10] != b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    // Year is 4 digits; read all four to support years > 2099 correctly.
    let year = std::str::from_utf8(b.get(0..4)?)
        .ok()?
        .parse::<i64>()
        .ok()?;
    let month = two(5)? as i64;
    let day = two(8)? as i64;
    let hour = two(11)?;
    let minute = two(14)?;
    let second = two(17)?;

    // Skip optional fractional seconds ".fff".
    let mut idx = 19;
    if idx < b.len() && b[idx] == b'.' {
        idx += 1;
        while idx < b.len() && b[idx].is_ascii_digit() {
            idx += 1;
        }
    }

    // Optional timezone: Z (UTC) or ±HH:MM offset. Absent ⇒ treat as UTC.
    let mut offset_secs: i64 = 0;
    if idx < b.len() {
        match b[idx] {
            b'Z' | b'z' => {}
            b'+' | b'-' => {
                let sign = if b[idx] == b'+' { 1 } else { -1 };
                if idx + 6 > b.len() || b[idx + 3] != b':' {
                    return None;
                }
                let oh = std::str::from_utf8(b.get(idx + 1..idx + 3)?)
                    .ok()?
                    .parse::<i64>()
                    .ok()?;
                let om = std::str::from_utf8(b.get(idx + 4..idx + 6)?)
                    .ok()?
                    .parse::<i64>()
                    .ok()?;
                offset_secs = sign * (oh * 3600 + om * 60);
            }
            _ => return None,
        }
    }

    let days = days_from_civil(year, month, day)?;
    let local_secs = days * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64);
    // Convert local → UTC by subtracting the offset, then clamp to non-negative.
    let utc = local_secs - offset_secs;
    if utc < 0 {
        None
    } else {
        Some(utc as u64)
    }
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01 for a proleptic
/// Gregorian date. Returns `None` for an out-of-range month/day so callers can't
/// feed it garbage.
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || day < 1 {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    Some(era * 146_097 + doe - 719_468)
}

// ---------------------------------------------------------------------------
// Fetch + poller
// ---------------------------------------------------------------------------

/// One result crossing the poller → main-loop channel.
#[derive(Debug, Clone)]
pub enum QuotaFetch {
    Report(QuotaReport),
    Failed(String),
}

/// Run `quota-axi --json` once, bounded by `timeout`. Never panics: every
/// failure (binary absent, timeout, non-zero exit, unparseable output) reduces
/// to [`QuotaFetch::Failed`].
///
/// **Pipe note:** stdout is read *after* the child exits. Today's payload is
/// ~8 KiB (well under the 64 KiB pipe buffer). If quota-axi ever emits more
/// than the buffer, the child blocks on the full pipe until the timeout SIGKILLs
/// it, after which the partial read fails to parse → `Failed`. Bounded badness;
/// accepted over the complexity of a concurrent stdout reader.
pub fn fetch_once(timeout: Duration) -> QuotaFetch {
    let mut cmd = match Command::new("quota-axi")
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::NotFound {
                "quota-axi not found".to_string()
            } else {
                format!("quota-axi spawn failed: {e}")
            };
            return QuotaFetch::Failed(msg);
        }
    };

    // Poll for exit up to `timeout`, then SIGKILL + reap. try_wait never blocks.
    let deadline = std::time::Instant::now() + timeout;
    let exit_status = loop {
        match cmd.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = cmd.kill();
                    let _ = cmd.wait();
                    break Err("timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = cmd.kill();
                let _ = cmd.wait();
                break Err(format!("quota-axi wait failed: {e}"));
            }
        }
    };

    let status = match exit_status {
        Ok(s) => s,
        Err(msg) => return QuotaFetch::Failed(msg),
    };
    if !status.success() {
        let stderr = cmd
            .stderr
            .take()
            .and_then(|s| std::io::read_to_string(s).ok())
            .unwrap_or_default();
        let first_line = stderr.lines().next().unwrap_or("").trim();
        let msg = if first_line.is_empty() {
            format!("quota-axi exited {status}")
        } else {
            // Truncate to keep the failure line readable.
            let truncated: String = first_line.chars().take(60).collect();
            truncated
        };
        return QuotaFetch::Failed(msg);
    }

    let stdout = cmd
        .stdout
        .take()
        .and_then(|s| std::io::read_to_string(s).ok())
        .unwrap_or_default();
    match parse_report(&stdout) {
        Ok(r) => QuotaFetch::Report(r),
        Err(e) => QuotaFetch::Failed(e),
    }
}

/// Spawn the background poller thread. Loops `fetch → send → sleep(interval)`;
/// exits cleanly when the receiver is dropped (send fails). Detached — process
/// exit reaps it. The fetch timeout is fixed at 10s regardless of cadence.
pub fn spawn_poller(interval: Duration, tx: Sender<QuotaFetch>) -> thread::JoinHandle<()> {
    thread::spawn(move || loop {
        let fetch = fetch_once(Duration::from_secs(10));
        if tx.send(fetch).is_err() {
            return; // receiver gone (app shutting down)
        }
        thread::sleep(interval);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed but structurally complete capture of `quota-axi --json` (schema 3)
    // covering the live shape (claude, 3 windows) and every failure shape seen on
    // the captain's machine today. Mirrors the meta.rs SAMPLE idiom.
    const SAMPLE: &str = r#"{
  "generatedAt": "2026-08-13T13:54:46.385Z",
  "schemaVersion": 3,
  "providers": [
    {
      "provider": "claude", "label": "Claude", "source": "oauth", "plan": "max",
      "windows": [
        { "id": "five_hour",   "label": "session",    "kind": "session",
          "percentUsed": 5,  "percentRemaining": 95,
          "resetsAt": "2026-08-13T15:40:00.000000+00:00", "windowSeconds": 18000,
          "pace": { "status": "behind" } },
        { "id": "seven_day",   "label": "week",       "kind": "weekly",
          "percentUsed": 48, "resetsAt": "2026-08-17T12:00:00.000000+00:00" },
        { "id": "model:fable", "label": "Fable week", "kind": "model",
          "percentUsed": 45, "resetsAt": "2026-08-17T12:00:00.000000+00:00" }
      ],
      "state": { "status": "fresh", "stale": false, "refreshedAt": "2026-08-13T13:54:46.846Z",
                 "sourcesTried": ["oauth-file", "oauth-profile"] },
      "quotaSemantics": { "status": "known" }
    },
    { "provider": "codex",   "label": "Codex",   "source": "unavailable", "windows": [],
      "state": { "status": "error", "stale": false, "error": "Codex quota unavailable" } },
    { "provider": "cursor",  "label": "Cursor",  "source": "unavailable", "windows": [],
      "state": { "status": "error", "stale": false, "error": "sqlite3_unavailable" } },
    { "provider": "copilot", "label": "GitHub Copilot", "source": "unavailable", "windows": [],
      "state": { "status": "auth_required", "stale": false, "error": "GitHub Copilot sign-in required" } },
    { "provider": "grok",    "label": "Grok",    "source": "unavailable", "windows": [],
      "state": { "status": "auth_required", "stale": false, "error": "Grok sign-in required" } },
    { "provider": "kimi",    "label": "Kimi",    "source": "unavailable", "windows": [],
      "state": { "status": "auth_required", "stale": false, "error": "kimi_credential_unavailable" } }
  ]
}"#;

    #[test]
    fn parse_full_fixture() {
        let r = parse_report(SAMPLE).expect("fixture must parse");
        assert_eq!(r.schema_version, Some(3));
        assert_eq!(r.providers.len(), 6);

        let claude = &r.providers[0];
        assert_eq!(claude.id, "claude");
        assert_eq!(claude.label, "Claude");
        assert_eq!(claude.plan.as_deref(), Some("max"));
        assert_eq!(claude.status, ProviderStatus::Fresh);
        assert!(!claude.stale);
        assert!(has_usage_windows(claude));
        assert_eq!(claude.windows.len(), 3);
        assert_eq!(claude.windows[0].id, "five_hour");
        assert_eq!(claude.windows[0].label, "session");
        assert!((claude.windows[0].percent_used - 5.0).abs() < 1e-9);
        assert_eq!(
            claude.windows[0].resets_at.as_deref(),
            Some("2026-08-13T15:40:00.000000+00:00")
        );
        // model: window id preserved verbatim (label rule is the row's job).
        assert_eq!(claude.windows[2].id, "model:fable");
    }

    #[test]
    fn parse_failure_shapes() {
        let r = parse_report(SAMPLE).unwrap();
        let by = |id: &str| r.providers.iter().find(|p| p.id == id).unwrap();
        assert_eq!(by("codex").status, ProviderStatus::Error);
        assert_eq!(
            by("codex").error.as_deref(),
            Some("Codex quota unavailable")
        );
        assert!(!has_usage_windows(by("codex")));
        assert_eq!(by("copilot").status, ProviderStatus::AuthRequired);
        assert!(!has_usage_windows(by("copilot")));
        // claude is the only provider carrying windows in the fixture
        let with_windows: Vec<&str> = r
            .providers
            .iter()
            .filter(|p| has_usage_windows(p))
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(with_windows, vec!["claude"]);
    }

    #[test]
    fn windows_visible_regardless_of_status() {
        // A provider serving cached windows (state.status = stale) or reporting a
        // status crew-watch has never seen must still count as renderable: status
        // is a display signal, not a visibility filter.
        let r = parse_report(
            r#"{ "providers": [
              { "provider": "claude", "windows": [ { "id": "five_hour", "percentUsed": 40 } ],
                "state": { "status": "stale", "stale": true } },
              { "provider": "zai", "windows": [ { "id": "five_hour", "percentUsed": 12 } ] }
            ] }"#,
        )
        .unwrap();
        assert_eq!(
            r.providers[0].status,
            ProviderStatus::Unknown("stale".to_string())
        );
        assert!(r.providers[0].stale);
        assert!(has_usage_windows(&r.providers[0]));
        // No `state` block at all ⇒ Unknown("missing"), still renderable.
        assert_eq!(
            r.providers[1].status,
            ProviderStatus::Unknown("missing".to_string())
        );
        assert!(has_usage_windows(&r.providers[1]));
    }

    #[test]
    fn parse_unknown_fields_ignored_and_optionals_default() {
        // Unknown top-level field, provider missing plan/state, window missing resetsAt.
        let mini = r#"{
          "schemaVersion": 3, "futureKey": 99,
          "providers": [
            { "provider": "zai", "label": "Z.AI",
              "windows": [ { "id": "five_hour", "label": "session", "percentUsed": 12 } ] }
          ]
        }"#;
        let r = parse_report(mini).unwrap();
        assert_eq!(r.providers.len(), 1);
        let p = &r.providers[0];
        assert_eq!(p.id, "zai");
        assert!(p.plan.is_none());
        assert_eq!(p.status, ProviderStatus::Unknown("missing".to_string()));
        assert!(p.windows[0].resets_at.is_none());
        assert!((p.windows[0].percent_used - 12.0).abs() < 1e-9);
    }

    #[test]
    fn parse_empty_providers_ok() {
        let r = parse_report(r#"{ "schemaVersion": 3, "providers": [] }"#).unwrap();
        assert!(r.providers.is_empty());
    }

    #[test]
    fn parse_garbage_returns_err_with_schema() {
        let err =
            parse_report(r#"{ "schemaVersion": 7, "providers": "not an array" }"#).unwrap_err();
        assert!(err.contains("schema v7"), "got: {err}");
    }

    #[test]
    fn parse_garbage_no_schema() {
        let err = parse_report("not json at all").unwrap_err();
        assert!(err.contains("no schema"), "got: {err}");
    }

    #[test]
    fn parse_unknown_status_preserved() {
        let r = parse_report(
            r#"{ "providers": [ { "provider": "x", "state": { "status": "weird_new_one" } } ] }"#,
        )
        .unwrap();
        assert_eq!(
            r.providers[0].status,
            ProviderStatus::Unknown("weird_new_one".to_string())
        );
    }

    // --- parse_iso8601_utc_epoch ---

    #[test]
    fn iso_zulu() {
        // 2026-08-13T13:55:00Z == 1786629300 (verified via the civil formula).
        assert_eq!(
            parse_iso8601_utc_epoch("2026-08-13T13:55:00Z"),
            Some(1_786_629_300)
        );
    }

    #[test]
    fn iso_zero_offset_same_as_zulu() {
        assert_eq!(
            parse_iso8601_utc_epoch("2026-08-13T13:55:00.000000+00:00"),
            Some(1_786_629_300)
        );
    }

    #[test]
    fn iso_positive_offset() {
        // 15:00 local at +02:00 == 13:00 UTC.
        assert_eq!(
            parse_iso8601_utc_epoch("2026-08-13T15:00:00+02:00"),
            parse_iso8601_utc_epoch("2026-08-13T13:00:00Z")
        );
    }

    #[test]
    fn iso_negative_offset() {
        // 10:30 local at -05:30 == 16:00 UTC.
        assert_eq!(
            parse_iso8601_utc_epoch("2026-08-13T10:30:00-05:30"),
            parse_iso8601_utc_epoch("2026-08-13T16:00:00Z")
        );
    }

    #[test]
    fn iso_fractional_seconds_ignored() {
        assert_eq!(
            parse_iso8601_utc_epoch("2026-08-17T12:00:00.547442+00:00"),
            parse_iso8601_utc_epoch("2026-08-17T12:00:00Z")
        );
    }

    #[test]
    fn iso_garbage_returns_none() {
        assert_eq!(parse_iso8601_utc_epoch("garbage"), None);
        assert_eq!(parse_iso8601_utc_epoch("2026-08-13"), None);
        assert_eq!(parse_iso8601_utc_epoch(""), None);
    }
}
