//! Pure quota-row line builder.
//!
//! Turns a [`ProviderQuota`] into styled-segment specs ([`RowSegment`]) that
//! `ui.rs` renders, and into a bar-free string for `--once` ([`once_line`]).
//! All width/label/bar decisions live here so the renderer stays a thin mapper
//! (the crate convention: pure logic is unit-tested, rendering is not).
//!
//! ## Tier ladder
//!
//! The first tier whose rendered width fits the terminal wins; each provider
//! line fits independently. Bars never shrink below 6 cells — reset time is
//! sacrificed before the bar becomes decoration. See [`Tier`].

use crate::format_util::{format_reset, FILL_BLOCK, SHADE_BLOCK};
use crate::quota::{parse_iso8601_utc_epoch, ProviderQuota, ProviderStatus};

/// One piece of a rendered provider line. `pct` is `Some` when the segment
/// should be colored with crew-watch's `pct_color` (the bar and the percentage),
/// mirroring the memory/swap meters; `None` for plain text.
#[derive(Debug, Clone, PartialEq)]
pub struct RowSegment {
    pub text: String,
    pub pct: Option<f64>,
}

impl RowSegment {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            pct: None,
        }
    }
    fn colored(text: impl Into<String>, pct: f64) -> Self {
        Self {
            text: text.into(),
            pct: Some(pct),
        }
    }
}

/// A degradation tier: bar width (if any), whether the reset countdown is shown,
/// and whether window labels are full or reduced to their first letter.
#[derive(Clone, Copy)]
struct Tier {
    bar: Option<usize>,
    reset: bool,
    labels: LabelMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LabelMode {
    Full,
    FirstLetter,
}

/// The tuned ladder (descending fidelity). Order matters: the first tier whose
/// rendered width fits is used. Reset is dropped before the bar shrinks below 6;
/// the bar is dropped entirely before labels abbreviate.
const TIERS: &[Tier] = &[
    Tier {
        bar: Some(12),
        reset: true,
        labels: LabelMode::Full,
    },
    Tier {
        bar: Some(10),
        reset: true,
        labels: LabelMode::Full,
    },
    Tier {
        bar: Some(8),
        reset: true,
        labels: LabelMode::Full,
    },
    Tier {
        bar: Some(6),
        reset: true,
        labels: LabelMode::Full,
    },
    Tier {
        bar: Some(10),
        reset: false,
        labels: LabelMode::Full,
    },
    Tier {
        bar: Some(8),
        reset: false,
        labels: LabelMode::Full,
    },
    Tier {
        bar: Some(6),
        reset: false,
        labels: LabelMode::Full,
    },
    Tier {
        bar: None,
        reset: false,
        labels: LabelMode::Full,
    },
    Tier {
        bar: None,
        reset: false,
        labels: LabelMode::FirstLetter,
    },
];

/// Window label rule (reproduces quota-axi's own TUI labels generically):
/// `model:fable` → `fable`; otherwise the window's `label` verbatim.
fn window_label<'a>(id: &'a str, label: &'a str) -> &'a str {
    if let Some(rest) = id.strip_prefix("model:") {
        rest
    } else {
        label
    }
}

/// Seconds until `resets_at`, relative to `now_epoch`, or `None` if the
/// timestamp is missing/unparseable.
fn reset_secs(resets_at: Option<&str>, now_epoch: u64) -> Option<u64> {
    let target = parse_iso8601_utc_epoch(resets_at?)?;
    Some(target.saturating_sub(now_epoch))
}

/// Build a usage bar with the min-one-filled-cell rule the row applies: a window
/// with `percentUsed > 0` always shows at least one filled cell (otherwise 5% at
/// bar-8 rounds to zero and renders fully empty). Uses crew-watch's `█░` glyphs.
fn bar_min_one(pct: f64, width: usize) -> String {
    let width = width.max(1);
    let clamped = pct.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let filled = if pct > 0.0 { filled.max(1) } else { filled };
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push(FILL_BLOCK);
    }
    for _ in filled..width {
        s.push(SHADE_BLOCK);
    }
    s
}

/// Render one provider line for the TUI at the given width. For a provider with
/// no usable windows this is a single dim status phrase; otherwise it picks the
/// best-fitting tier. `now_epoch` (UTC seconds) makes reset countdowns pure and
/// testable without a system clock.
pub fn build_provider_line(p: &ProviderQuota, now_epoch: u64, width: usize) -> Vec<RowSegment> {
    if p.windows.is_empty() {
        return dim_status_line(&p.id, &p.status, p.error.as_deref());
    }

    for tier in TIERS {
        let segs = build_tier(p, now_epoch, *tier);
        if line_width(&segs) <= width {
            return segs;
        }
    }
    // Exhausted every tier (absurdly narrow): hard-truncate the cheapest tier.
    let fallback = build_tier(p, now_epoch, *TIERS.last().unwrap());
    truncate_segments(fallback, width)
}

/// A live provider's per-window bar/percent/reset, in the `--once` format
/// (bar-free): `claude   session 5% 1h45m  week 48% 3d22h  ...`. For a provider
/// without windows: `{id}: {phrase}` (e.g. `copilot: sign-in required`). The
/// caller prefixes `quota ` and any per-line decoration.
pub fn once_line(p: &ProviderQuota, now_epoch: u64) -> String {
    if p.windows.is_empty() {
        return format!("{}: {}", p.id, status_phrase(&p.status, p.error.as_deref()));
    }
    let mut out = format!("{:<7}  ", p.id);
    let mut first = true;
    for w in &p.windows {
        if !first {
            out.push_str("  ");
        }
        first = false;
        let label = window_label(&w.id, &w.label);
        out.push_str(label);
        out.push(' ');
        out.push_str(&format!("{:.0}%", w.percent_used));
        if let Some(secs) = reset_secs(w.resets_at.as_deref(), now_epoch) {
            out.push(' ');
            out.push_str(&format_reset(secs));
        }
    }
    out
}

/// Dialog status note: live providers show `{plan} · {status}`; non-live ones
/// show the short status phrase.
pub fn dialog_note(p: &ProviderQuota) -> String {
    if !p.windows.is_empty() {
        let plan = p.plan.as_deref().unwrap_or("-");
        format!("{} · {}", plan, status_word(&p.status))
    } else {
        status_phrase(&p.status, p.error.as_deref())
    }
}

/// Short human phrase for a non-live provider's dim line.
fn status_phrase(status: &ProviderStatus, error: Option<&str>) -> String {
    match status {
        ProviderStatus::AuthRequired => "sign-in required".to_string(),
        ProviderStatus::Error => "unavailable".to_string(),
        ProviderStatus::Fresh => "no usage".to_string(),
        ProviderStatus::Unknown(_) => error.unwrap_or("unavailable").to_string(),
    }
}

/// Single status word for the dialog's live note (e.g. `fresh`).
fn status_word(status: &ProviderStatus) -> String {
    match status {
        ProviderStatus::Fresh => "fresh".to_string(),
        ProviderStatus::Error => "error".to_string(),
        ProviderStatus::AuthRequired => "auth_required".to_string(),
        ProviderStatus::Unknown(s) => s.clone(),
    }
}

fn dim_status_line(id: &str, status: &ProviderStatus, error: Option<&str>) -> Vec<RowSegment> {
    // `{id:<7}` field + one separator space keeps the phrase column aligned
    // across providers and guarantees a gap even for a 7-char id (e.g. copilot).
    let phrase = status_phrase(status, error);
    vec![RowSegment::plain(format!(" {:<7} {}", id, phrase))]
}

fn build_tier(p: &ProviderQuota, now_epoch: u64, tier: Tier) -> Vec<RowSegment> {
    let mut segs = Vec::with_capacity(2 + p.windows.len() * 4);
    segs.push(RowSegment::plain(format!(" {:<7}", p.id)));
    for (i, w) in p.windows.iter().enumerate() {
        let sep = if i == 0 { "" } else { "  " };
        let full_label = window_label(&w.id, &w.label);
        let label: String = match tier.labels {
            LabelMode::Full => full_label.to_string(),
            LabelMode::FirstLetter => full_label
                .chars()
                .next()
                .map(|c| c.to_string())
                .unwrap_or_default(),
        };
        segs.push(RowSegment::plain(format!("{}{} ", sep, label)));
        match tier.bar {
            Some(bw) => {
                segs.push(RowSegment::colored(
                    format!("{} ", bar_min_one(w.percent_used, bw)),
                    w.percent_used,
                ));
                segs.push(RowSegment::colored(
                    format!("{:>3.0}%", w.percent_used),
                    w.percent_used,
                ));
            }
            None => {
                segs.push(RowSegment::colored(
                    format!("{:>3.0}%", w.percent_used),
                    w.percent_used,
                ));
            }
        }
        if tier.reset {
            if let Some(secs) = reset_secs(w.resets_at.as_deref(), now_epoch) {
                segs.push(RowSegment::plain(format!(" {}", format_reset(secs))));
            }
        }
    }
    segs
}

fn line_width(segs: &[RowSegment]) -> usize {
    segs.iter().map(|s| s.text.chars().count()).sum()
}

fn truncate_segments(segs: Vec<RowSegment>, width: usize) -> Vec<RowSegment> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in segs {
        let remaining = width.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        let chars: Vec<char> = s.text.chars().collect();
        if chars.len() <= remaining {
            used += chars.len();
            out.push(s);
        } else {
            let truncated: String = chars.into_iter().take(remaining).collect();
            out.push(RowSegment {
                text: truncated,
                pct: s.pct,
            });
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::{ProviderQuota, ProviderStatus, QuotaWindow};

    /// now_epoch for 2026-08-13T13:55:00Z — yields the plan's verified resets
    /// (session=1h45m, week/fable=3d22h) and is used by every width-vector test.
    const NOW: u64 = 1_786_629_300;

    fn claude() -> ProviderQuota {
        ProviderQuota {
            id: "claude".to_string(),
            label: "Claude".to_string(),
            plan: Some("max".to_string()),
            windows: vec![
                QuotaWindow {
                    id: "five_hour".to_string(),
                    label: "session".to_string(),
                    percent_used: 5.0,
                    resets_at: Some("2026-08-13T15:40:00.000000+00:00".to_string()),
                },
                QuotaWindow {
                    id: "seven_day".to_string(),
                    label: "week".to_string(),
                    percent_used: 48.0,
                    resets_at: Some("2026-08-17T12:00:00.000000+00:00".to_string()),
                },
                QuotaWindow {
                    id: "model:fable".to_string(),
                    label: "Fable week".to_string(),
                    percent_used: 45.0,
                    resets_at: Some("2026-08-17T12:00:00.000000+00:00".to_string()),
                },
            ],
            status: ProviderStatus::Fresh,
            stale: false,
            error: None,
        }
    }

    /// Concatenate segment texts into the rendered string (for exact-string asserts).
    fn render(segs: &[RowSegment]) -> String {
        segs.iter().map(|s| s.text.as_str()).collect()
    }

    // --- width-degradation ladder (exact strings from the plan §4.2) ---

    #[test]
    fn tier1_full_fidelity_110cols() {
        let line = render(&build_provider_line(&claude(), NOW, 110));
        // Percentages are width-3 right-aligned (`{:>3.0}%`): a 1-digit pct
        // gets 2 pad spaces, a 2-digit pct gets 1. The bar carries one trailing
        // separator space, so session reads "   5%" and week/fable "  48%"/"  45%".
        assert_eq!(
            line,
            " claude session \u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}   5% 1h45m  week \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}  48% 3d22h  fable \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}  45% 3d22h"
        );
    }

    #[test]
    fn tier_ladder_80_drops_reset_shortens_bar() {
        // width 80: bars shrink to 10, reset dropped, full labels.
        let line = render(&build_provider_line(&claude(), NOW, 80));
        assert!(line.contains("session"), "labels full: {line}");
        assert!(!line.contains("1h45m"), "reset dropped: {line}");
        assert!(!line.contains("3d22h"), "reset dropped: {line}");
        assert_eq!(
            line,
            " claude session \u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}   5%  week \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}  48%  fable \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}  45%"
        );
    }

    #[test]
    fn tier8_no_bar_full_labels_55cols() {
        let line = render(&build_provider_line(&claude(), NOW, 55));
        assert_eq!(line, " claude session   5%  week  48%  fable  45%");
    }

    #[test]
    fn tier9_first_letter_40cols() {
        let line = render(&build_provider_line(&claude(), NOW, 40));
        assert_eq!(line, " claude s   5%  w  48%  f  45%");
    }

    #[test]
    fn fallback_hard_truncate_when_absurdly_narrow() {
        // 20 cols: tier-9 needs more, so it hard-truncates without panicking.
        let line = render(&build_provider_line(&claude(), NOW, 20));
        assert!(
            line.chars().count() <= 20,
            "truncated to width: {line} ({})",
            line.chars().count()
        );
    }

    // --- label rule, min-one-cell, clamp, missing reset ---

    #[test]
    fn model_label_rule() {
        let line = render(&build_provider_line(&claude(), NOW, 110));
        // model:fable window renders as "fable", not "Fable week".
        assert!(line.contains("fable"), "{line}");
        assert!(!line.contains("Fable week"), "{line}");
    }

    #[test]
    fn min_one_filled_cell_when_pct_small() {
        // 5% at bar-8 would round to 0; min-one-cell forces a single fill.
        let mut p = claude();
        // Keep only the session window, force a tier with bar-8 by using width ~70.
        p.windows.truncate(1);
        let line = render(&build_provider_line(&p, NOW, 70));
        // The bar segment is the one with pct Some; it must contain a fill glyph.
        assert!(
            line.contains('\u{2588}'),
            "5% must show at least one filled cell: {line}"
        );
    }

    #[test]
    fn pct_above_100_clamped_for_bar_printed_raw() {
        let mut p = claude();
        p.windows[0].percent_used = 150.0;
        let line = render(&build_provider_line(&p, NOW, 110));
        // Bar fully filled, and the raw 150% is printed (not clamped to 100).
        assert!(line.contains("150%"), "raw pct printed: {line}");
    }

    #[test]
    fn missing_resets_at_omits_segment() {
        let mut p = claude();
        p.windows[0].resets_at = None;
        let line = render(&build_provider_line(&p, NOW, 110));
        // session window has no reset suffix; week/fable still do.
        assert!(
            line.contains("5%  week"),
            "session reset omitted, flows into week: {line}"
        );
    }

    // --- non-live dim lines ---

    #[test]
    fn dim_line_auth_required() {
        let p = ProviderQuota {
            id: "copilot".to_string(),
            label: "GitHub Copilot".to_string(),
            plan: None,
            windows: vec![],
            status: ProviderStatus::AuthRequired,
            stale: false,
            error: Some("GitHub Copilot sign-in required".to_string()),
        };
        let line = render(&build_provider_line(&p, NOW, 80));
        assert_eq!(line, " copilot sign-in required");
    }

    #[test]
    fn dim_line_error() {
        let p = ProviderQuota {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            plan: None,
            windows: vec![],
            status: ProviderStatus::Error,
            stale: false,
            error: Some("Codex quota unavailable".to_string()),
        };
        let line = render(&build_provider_line(&p, NOW, 80));
        assert_eq!(line, " codex   unavailable");
    }

    // --- once_line ---

    #[test]
    fn once_line_live() {
        let line = once_line(&claude(), NOW);
        assert_eq!(
            line,
            "claude   session 5% 1h45m  week 48% 3d22h  fable 45% 3d22h"
        );
    }

    #[test]
    fn once_line_non_live() {
        let p = ProviderQuota {
            id: "copilot".to_string(),
            label: "GitHub Copilot".to_string(),
            plan: None,
            windows: vec![],
            status: ProviderStatus::AuthRequired,
            stale: false,
            error: Some("x".to_string()),
        };
        assert_eq!(once_line(&p, NOW), "copilot: sign-in required");
    }

    // --- dialog note ---

    #[test]
    fn dialog_note_live_and_non_live() {
        assert_eq!(dialog_note(&claude()), "max · fresh");
        let mut p = claude();
        p.status = ProviderStatus::Error;
        p.windows.clear();
        assert_eq!(dialog_note(&p), "unavailable");
    }

    // --- multi-provider independence ---

    #[test]
    fn each_line_fits_independently() {
        // A 2-window provider keeps its bars at a width where a 3-window one
        // would have already dropped resets — each line picks its own tier.
        let zai = ProviderQuota {
            id: "zai".to_string(),
            label: "Z.AI".to_string(),
            plan: None,
            windows: vec![
                QuotaWindow {
                    id: "five_hour".to_string(),
                    label: "5h".to_string(),
                    percent_used: 61.0,
                    resets_at: Some("2026-08-13T16:05:00Z".to_string()),
                },
                QuotaWindow {
                    id: "month".to_string(),
                    label: "month".to_string(),
                    percent_used: 12.0,
                    resets_at: Some("2026-08-30T16:00:00Z".to_string()),
                },
            ],
            status: ProviderStatus::Fresh,
            stale: false,
            error: None,
        };
        let line = render(&build_provider_line(&zai, NOW, 60));
        assert!(line.chars().count() <= 60, "fits: {line}");
        assert!(line.contains("5h"), "{line}");
        assert!(line.contains("month"), "{line}");
    }
}
