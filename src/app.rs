//! Application state owned by the main loop.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::detect::Session;
use crate::fleet::Sampler;
use crate::procfs::Snapshot;
use crate::quota::{has_usage_windows, QuotaFetch, QuotaReport};

pub struct App {
    pub interval: Duration,
    /// The data pipeline over the firstmate home. It re-reads fleet records
    /// and task titles on every tick so a long-running instance tracks task
    /// lifecycle instead of freezing whatever was on disk at launch. See the
    /// `src/taskinfo.rs` header for why this freshness matters.
    pub sampler: Sampler,
    // --- quota row state (never touched by tick(); see src/quota.rs) ---
    /// False under `--no-quota`: the row is absent and nothing is fetched.
    pub quota_enabled: bool,
    /// Poller cadence (clamped 60..=3600s). Used for the staleness threshold.
    pub quota_interval: Duration,
    /// `None` ⇒ auto mode (providers with ≥1 window). `Some(ids)` ⇒ explicit.
    pub quota_selected: Option<Vec<String>>,
    /// Channel from the background poller; `None` when disabled / `--once`.
    pub quota_rx: Option<Receiver<QuotaFetch>>,
    pub quota: QuotaState,
    /// Transient one-line message shown in the help line until next keypress
    /// (e.g. a config-save failure). Cleared by any key in the main loop.
    pub notice: Option<String>,
    /// Provider-selection dialog; `Some` while open, rendered as an overlay.
    pub dialog: Option<crate::quota_dialog::QuotaDialog>,
    /// About overlay; `true` while open. A view, not a mode: ticks and quota
    /// updates continue underneath while it is up.
    pub show_about: bool,
}

/// Last-known quota fetch state. On a [`QuotaFetch::Failed`] the previous report
/// is retained (rendered dim) so a transient outage never blanks the row.
#[derive(Default)]
pub struct QuotaState {
    pub report: Option<QuotaReport>,
    pub last_ok_at: Option<Instant>,
    pub last_error: Option<String>,
}

/// The effective provider selection driving the row and `--once`.
#[derive(Debug, Clone)]
pub enum QuotaSelection {
    /// Show every provider that reports ≥1 window.
    Auto,
    /// Show exactly these provider ids (report order at render time).
    Explicit(Vec<String>),
}

impl App {
    pub fn new(interval: Duration, fm_home: PathBuf) -> Self {
        Self {
            interval,
            sampler: Sampler::new(fm_home),
            quota_enabled: true,
            quota_interval: Duration::from_secs(600),
            quota_selected: None,
            quota_rx: None,
            quota: QuotaState::default(),
            notice: None,
            dialog: None,
            show_about: false,
        }
    }

    /// Collect one /proc snapshot, rebuild sessions, and resolve task info.
    pub fn tick(&mut self) {
        self.sampler.refresh();
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.sampler.snapshot()
    }

    pub fn prev_snapshot(&self) -> Option<&Snapshot> {
        self.sampler.prev_snapshot()
    }

    pub fn sessions(&self) -> &[Session] {
        self.sampler.sessions()
    }

    // --- quota (none of this rides the /proc tick path) ---

    /// Drain all pending quota fetches from the background poller, non-blocking.
    /// A [`QuotaFetch::Report`] updates state and refreshes `last_ok_at`; a
    /// [`QuotaFetch::Failed`] records the error but keeps the last good report
    /// so the row dims rather than vanishing.
    pub fn drain_quota(&mut self) {
        let Some(rx) = &self.quota_rx else {
            return;
        };
        while let Ok(fetch) = rx.try_recv() {
            match fetch {
                QuotaFetch::Report(r) => {
                    self.quota.report = Some(r);
                    self.quota.last_ok_at = Some(Instant::now());
                    self.quota.last_error = None;
                }
                QuotaFetch::Failed(e) => {
                    self.quota.last_error = Some(e);
                }
            }
        }
    }

    /// The effective provider selection: explicit (from config) or auto.
    pub fn effective_selection(&self) -> QuotaSelection {
        match &self.quota_selected {
            Some(ids) => QuotaSelection::Explicit(ids.clone()),
            None => QuotaSelection::Auto,
        }
    }

    /// Number of quota lines to render this frame (the height of the quota slot).
    /// 0 when disabled, auto with no provider reporting windows, or an
    /// explicit-empty selection — in all those cases the slot vanishes and the
    /// layout is byte-identical to pre-quota crew-watch.
    pub fn quota_lines_count(&self) -> usize {
        if !self.quota_enabled {
            return 0;
        }
        match self.effective_selection() {
            QuotaSelection::Explicit(ids) => {
                if ids.is_empty() {
                    return 0; // explicit empty ⇒ row hidden
                }
                match &self.quota.report {
                    Some(r) => r.providers.iter().filter(|p| ids.contains(&p.id)).count(),
                    None => 1, // fetch-level unavailable line (D9: explicit ⇒ visible failure)
                }
            }
            QuotaSelection::Auto => self
                .quota
                .report
                .as_ref()
                .map(|r| r.providers.iter().filter(|p| has_usage_windows(p)).count())
                .unwrap_or(0),
        }
    }

    /// The fetch age, if the row should be rendered dim because the poller has
    /// missed roughly two consecutive intervals (`age > 3 × cadence`). The
    /// threshold is relative to cadence so a 5-minute cadence stays "fresh"
    /// looking for ~15 minutes — it never reads as perpetually stale.
    pub fn quota_fetch_age(&self) -> Option<Duration> {
        let ok_at = self.quota.last_ok_at?;
        let age = ok_at.elapsed();
        if age > self.quota_interval * 3 {
            Some(age)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- quota selection / line counting (D7, D9 legs) ---

    use crate::quota::{ProviderQuota, ProviderStatus, QuotaFetch, QuotaReport, QuotaWindow};

    fn quota_report_with(claude_live: bool) -> QuotaReport {
        let claude = if claude_live {
            ProviderQuota {
                id: "claude".to_string(),
                label: "Claude".to_string(),
                plan: Some("max".to_string()),
                windows: vec![QuotaWindow {
                    id: "five_hour".to_string(),
                    label: "session".to_string(),
                    percent_used: 5.0,
                    resets_at: None,
                }],
                status: ProviderStatus::Fresh,
                stale: false,
                error: None,
            }
        } else {
            ProviderQuota {
                id: "claude".to_string(),
                label: "Claude".to_string(),
                plan: None,
                windows: vec![],
                status: ProviderStatus::Error,
                stale: false,
                error: Some("x".to_string()),
            }
        };
        QuotaReport {
            schema_version: Some(3),
            generated_at: String::new(),
            providers: vec![claude],
        }
    }

    #[test]
    fn effective_selection_auto_vs_explicit() {
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        assert!(matches!(app.effective_selection(), QuotaSelection::Auto));
        app.quota_selected = Some(vec!["claude".to_string()]);
        assert!(matches!(
            app.effective_selection(),
            QuotaSelection::Explicit(_)
        ));
    }

    #[test]
    fn quota_lines_auto_live_then_failed() {
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        // auto, no report yet -> 0 lines (row absent until first fetch lands)
        assert_eq!(app.quota_lines_count(), 0);
        app.quota.report = Some(quota_report_with(true));
        assert_eq!(app.quota_lines_count(), 1, "live claude -> 1 line");
        // provider loses its windows: auto has nothing to render
        app.quota.report = Some(quota_report_with(false));
        assert_eq!(app.quota_lines_count(), 0);
    }

    #[test]
    fn quota_lines_auto_counts_stale_and_unknown_status() {
        // Staleness (cache fallback) and an unrecognised status must not blank the
        // row: as long as a provider reports windows, auto mode gives it a line.
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        let mut report = quota_report_with(true);
        report.providers[0].stale = true;
        report.providers[0].status = ProviderStatus::Unknown("stale".to_string());
        app.quota.report = Some(report);
        assert_eq!(app.quota_lines_count(), 1);
    }

    #[test]
    fn quota_lines_explicit_d9_legs() {
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        // explicit empty -> row hidden
        app.quota_selected = Some(vec![]);
        assert_eq!(app.quota_lines_count(), 0);
        // explicit selection, no report -> one unavailable line (D9)
        app.quota_selected = Some(vec!["claude".to_string()]);
        assert_eq!(app.quota_lines_count(), 1);
        // explicit selection, report present -> the matching provider line
        app.quota.report = Some(quota_report_with(true));
        assert_eq!(app.quota_lines_count(), 1);
        // explicit selection of a vanished provider (not in report) -> omitted
        app.quota_selected = Some(vec!["zai".to_string()]);
        assert_eq!(app.quota_lines_count(), 0);
    }

    #[test]
    fn quota_lines_disabled_is_zero() {
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        app.quota_enabled = false;
        app.quota.report = Some(quota_report_with(true));
        app.quota_selected = Some(vec!["claude".to_string()]);
        assert_eq!(app.quota_lines_count(), 0);
    }

    #[test]
    fn drain_quota_keeps_last_report_on_failure() {
        use std::sync::mpsc::channel;
        let (tx, rx) = channel::<QuotaFetch>();
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        app.quota_rx = Some(rx);

        tx.send(QuotaFetch::Report(quota_report_with(true)))
            .unwrap();
        app.drain_quota();
        assert!(app.quota.report.is_some());
        assert!(app.quota.last_ok_at.is_some());

        // A subsequent failure must NOT wipe the last good report.
        tx.send(QuotaFetch::Failed("timed out".to_string()))
            .unwrap();
        app.drain_quota();
        assert!(
            app.quota.report.is_some(),
            "last report retained on failure"
        );
        assert_eq!(app.quota.last_error.as_deref(), Some("timed out"));
    }

    #[test]
    fn drain_quota_without_receiver_is_noop() {
        // --once path: no poller, no receiver. Must not panic.
        let mut app = App::new(Duration::from_secs(2), PathBuf::from("/nonexistent"));
        app.drain_quota();
        assert!(app.quota.report.is_none());
    }
}
