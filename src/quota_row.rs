//! Pure quota-row line builder.
//!
//! Turns [`ProviderQuota`]s into styled-segment specs ([`RowSegment`]) that
//! `ui.rs` renders, and into a bar-free string for `--once` ([`once_line`]).
//! All width/label/bar decisions live here so the renderer stays a thin mapper
//! (the crate convention: pure logic is unit-tested, rendering is not).
//!
//! ## Canonical window order
//!
//! Windows always render in a provider-independent order (see
//! [`sorted_windows`]/[`window_rank`]): `session`, then `week`, then the
//! provider's extra windows (anything that is neither) sorted alphabetically by
//! displayed label. Classification is by the displayed label, so a new provider
//! whose windows are labelled `session`/`week` falls into order on its own — no
//! per-provider list. This applies to both the TUI rows and `--once`.
//!
//! ## Aligned block + tier ladder
//!
//! When several providers show at once, [`build_quota_rows`] renders them as one
//! aligned block: every row shares a tier and per-column widths (label and
//! reset fields padded to the block max), so bars line up down the rows even
//! when labels or reset widths differ. The first tier whose widest aligned row
//! fits the terminal wins. If no aligned tier fits, alignment is dropped first
//! (it is the cheapest fidelity to lose) and each row degrades independently —
//! exactly the single-row [`build_provider_line`] ladder. Bars never shrink
//! below 6 cells; reset is sacrificed before the bar becomes decoration. A
//! single provider is never padded for a column no other row needs. See
//! [`Tier`].

use crate::format_util::{format_reset, make_bar_min_one};
use crate::quota::{
    has_usage_windows, parse_iso8601_utc_epoch, ProviderQuota, ProviderStatus, QuotaWindow,
};

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

/// Canonical rank for a window's *displayed* label: `session` (the short rolling
/// window) sorts first, then `week`, then every other window (the provider's
/// extras). Classification is by the displayed label so any provider whose
/// windows are labelled `session`/`week` orders correctly with no per-provider
/// special-casing — a new provider falls into order on its own.
fn window_rank(displayed_label: &str) -> u8 {
    match displayed_label {
        "session" => 0,
        "week" => 1,
        _ => 2,
    }
}

/// The provider's windows in canonical display order, by reference: `session`,
/// then `week`, then extras. Extras (any window that is neither `session` nor
/// `week`) are sorted alphabetically by displayed label for a stable order that
/// does not depend on the API's return order; ties keep source order (slice
/// `sort_by` is stable).
fn sorted_windows(windows: &[QuotaWindow]) -> Vec<&QuotaWindow> {
    let mut v: Vec<&QuotaWindow> = windows.iter().collect();
    v.sort_by(|a, b| {
        let la = window_label(&a.id, &a.label);
        let lb = window_label(&b.id, &b.label);
        window_rank(la)
            .cmp(&window_rank(lb))
            .then_with(|| la.cmp(lb))
    });
    v
}

/// Assign each window a block column index so the same kind lines up across
/// providers regardless of how many extras each carries: `session`→0, `week`→1,
/// an extra→2 + its alphabetical position among this provider's own extras.
/// Returns the windows in canonical order, tagged with their column.
fn provider_columns(windows: &[QuotaWindow]) -> Vec<(usize, &QuotaWindow)> {
    let sorted = sorted_windows(windows);
    let mut extras_seen = 0usize;
    sorted
        .into_iter()
        .map(|w| {
            let col = match window_label(&w.id, &w.label) {
                "session" => 0,
                "week" => 1,
                _ => {
                    let c = 2 + extras_seen;
                    extras_seen += 1;
                    c
                }
            };
            (col, w)
        })
        .collect()
}

/// Window label text for a tier: full, or reduced to its first letter on the
/// narrowest tier.
fn tier_label(full: &str, mode: LabelMode) -> String {
    match mode {
        LabelMode::Full => full.to_string(),
        LabelMode::FirstLetter => full
            .chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_default(),
    }
}

/// Seconds until `resets_at`, relative to `now_epoch`, or `None` if the
/// timestamp is missing/unparseable.
fn reset_secs(resets_at: Option<&str>, now_epoch: u64) -> Option<u64> {
    let target = parse_iso8601_utc_epoch(resets_at?)?;
    Some(target.saturating_sub(now_epoch))
}

/// Render one provider line for the TUI at the given width. For a provider with
/// no usable windows this is a single dim status phrase; otherwise it picks the
/// best-fitting tier. Windows are always in canonical order (session, week,
/// extras). `now_epoch` (UTC seconds) makes reset countdowns pure and testable
/// without a system clock.
///
/// This is the single-row entry point; for aligned multi-provider rendering use
/// [`build_quota_rows`] (one provider alone is never padded for a column no
/// other row needs).
pub fn build_provider_line(p: &ProviderQuota, now_epoch: u64, width: usize) -> Vec<RowSegment> {
    if !has_usage_windows(p) {
        return dim_status_line(&p.id, &p.status, p.error.as_deref());
    }
    build_row_independent(p, now_epoch, width)
}

/// Render the quota block: one row per provider, in input order, with bars
/// aligned down the rows. Live providers (those with windows) share one tier and
/// per-column widths computed from the rows actually shown; providers without
/// windows render their dim status line independently (no bars to align).
///
/// At wide terminals every live row uses the same tier, so each window kind
/// starts at the same column and bars line up even when labels or reset widths
/// differ. If no aligned tier fits the terminal, alignment is dropped first —
/// each live row then degrades through the [`build_provider_line`] ladder on its
/// own, exactly as a cramped terminal does today (no wrapping or overflow). With
/// a single live provider the column widths are that row's own, so nothing is
/// padded for an absent neighbour.
pub fn build_quota_rows(
    providers: &[&ProviderQuota],
    now_epoch: u64,
    width: usize,
) -> Vec<Vec<RowSegment>> {
    let live: Vec<&ProviderQuota> = providers
        .iter()
        .filter(|p| has_usage_windows(p))
        .copied()
        .collect();
    let aligned = if live.is_empty() {
        None
    } else {
        build_aligned_block(&live, now_epoch, width)
    };
    let mut live_idx = 0;
    providers
        .iter()
        .map(|p| {
            if !has_usage_windows(p) {
                dim_status_line(&p.id, &p.status, p.error.as_deref())
            } else {
                let row = match &aligned {
                    Some(block) => block[live_idx].clone(),
                    // Aligned block did not fit: drop alignment and let each row
                    // degrade through the single-row ladder on its own.
                    None => build_provider_line(p, now_epoch, width),
                };
                live_idx += 1;
                row
            }
        })
        .collect()
}

/// A live provider's per-window bar/percent/reset, in the `--once` format
/// (bar-free): `claude   session 5% 1h45m  week 48% 3d22h  ...`. Windows are
/// emitted in canonical order (session, week, extras). For a provider without
/// windows: `{id}: {phrase}` (e.g. `copilot: sign-in required`). The caller
/// prefixes `quota ` and any per-line decoration.
pub fn once_line(p: &ProviderQuota, now_epoch: u64) -> String {
    if !has_usage_windows(p) {
        return format!("{}: {}", p.id, status_phrase(&p.status, p.error.as_deref()));
    }
    let mut out = format!("{:<7}  ", p.id);
    let mut first = true;
    for w in sorted_windows(&p.windows) {
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
    if has_usage_windows(p) {
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

// ---------------------------------------------------------------------------
// Aligned block layout (Fault 2): shared tier + shared per-column widths so
// bars line up down the rows. Single-provider rendering goes through the same
// helpers with a one-row block, so it is never padded for an absent neighbour.
// ---------------------------------------------------------------------------

/// Per-column widths shared across every row in the block.
struct ColCols {
    /// Max displayed-label width at this column (labels are left-justified to
    /// it, so the bar that follows starts at the same x in every row).
    label_w: usize,
    /// Max reset-string width at this column (`0` if no row has a reset here).
    reset_w: usize,
}

struct BlockLayout {
    /// `{id:<id_w}` — the provider-id column width (floor 6 to keep the existing
    /// spacing for short ids).
    id_w: usize,
    /// One entry per block column `0..=block_max_col`.
    cols: Vec<ColCols>,
}

/// Compute the shared column widths for `provider_cols` at `tier`.
fn compute_layout(
    provider_cols: &[Vec<(usize, &QuotaWindow)>],
    id_w: usize,
    block_max_col: usize,
    tier: Tier,
    now_epoch: u64,
) -> BlockLayout {
    let mut cols: Vec<ColCols> = (0..=block_max_col)
        .map(|_| ColCols {
            label_w: 0,
            reset_w: 0,
        })
        .collect();
    for pc in provider_cols {
        for (c, w) in pc {
            let full = window_label(&w.id, &w.label);
            let label_text = tier_label(full, tier.labels);
            cols[*c].label_w = cols[*c].label_w.max(label_text.chars().count());
            if tier.reset {
                if let Some(secs) = reset_secs(w.resets_at.as_deref(), now_epoch) {
                    cols[*c].reset_w = cols[*c].reset_w.max(format_reset(secs).chars().count());
                }
            }
        }
    }
    BlockLayout { id_w, cols }
}

/// Render one provider's row into the shared `layout` at `tier`. Columns the
/// provider lacks are blank-padded only when the provider still has a later
/// column AND some other row actually fills that column (so the blank is
/// alignment padding, never gratuitous on a lone provider); trailing columns it
/// lacks are omitted (no needless blanks).
fn render_aligned_row(
    provider_cols: &[(usize, &QuotaWindow)],
    p: &ProviderQuota,
    layout: &BlockLayout,
    block_max_col: usize,
    column_used: &[bool],
    tier: Tier,
    now_epoch: u64,
) -> Vec<RowSegment> {
    let mut segs: Vec<RowSegment> = Vec::new();
    // id prefix: " {id:<id_w} "
    let id_field = format!("{:<w$}", p.id, w = layout.id_w);
    segs.push(RowSegment::plain(format!(" {} ", id_field)));

    let barpart = tier.bar.map(|bw| bw + 1).unwrap_or(0);

    for c in 0..=block_max_col {
        let sep = if c == 0 { "" } else { "  " };
        let win = provider_cols
            .iter()
            .find(|(col, _)| *col == c)
            .map(|(_, w)| *w);
        let provider_has_later_col = provider_cols.iter().any(|(col, _)| *col > c);

        if let Some(w) = win {
            let full = window_label(&w.id, &w.label);
            let label_text = tier_label(full, tier.labels);
            let label_w = layout.cols[c].label_w;
            // label segment: "{sep}{label:<label_w} " — padding the label (not
            // the whole slot) is what makes the bar line up across rows whose
            // labels differ in width.
            segs.push(RowSegment::plain(format!(
                "{}{:<lw$} ",
                sep,
                label_text,
                lw = label_w
            )));
            if let Some(bw) = tier.bar {
                segs.push(RowSegment::colored(
                    format!("{} ", make_bar_min_one(w.percent_used, bw)),
                    w.percent_used,
                ));
            }
            segs.push(RowSegment::colored(
                format!("{:>3.0}%", w.percent_used),
                w.percent_used,
            ));
            if tier.reset {
                let reset_str = reset_secs(w.resets_at.as_deref(), now_epoch).map(format_reset);
                let max_reset_len = layout.cols[c].reset_w;
                // Pad the reset field only on interior columns, so the next
                // column lines up; on the block's last column it is verbatim
                // (nothing follows to align, and this avoids trailing spaces).
                let interior = c < block_max_col && max_reset_len > 0;
                match (reset_str.as_ref(), provider_has_later_col) {
                    (Some(r), _) => {
                        if interior {
                            segs.push(RowSegment::plain(format!(
                                " {:<rw$}",
                                r,
                                rw = max_reset_len
                            )));
                        } else {
                            segs.push(RowSegment::plain(format!(" {}", r)));
                        }
                    }
                    (None, true) if interior => {
                        // No reset for this window, but a later column needs the
                        // field width held constant: placeholder spaces.
                        segs.push(RowSegment::plain(format!(" {}", " ".repeat(max_reset_len))));
                    }
                    (None, _) => {}
                }
            }
        } else if provider_has_later_col && column_used.get(c).copied().unwrap_or(false) {
            // No window at this column for this provider, but it has a later
            // column AND another row fills this column: blank the whole column
            // (incl. its reset-field width) so the provider's next column still
            // starts at the shared x. (If no other row uses this column there is
            // nothing to align against, so a lone or short provider is not padded
            // for a column no other row needs.)
            let label_w = layout.cols[c].label_w;
            let max_reset_len = layout.cols[c].reset_w;
            let reset_field = if tier.reset && c < block_max_col && max_reset_len > 0 {
                1 + max_reset_len
            } else {
                0
            };
            let content_w = label_w + 1 + barpart + 4 + reset_field;
            segs.push(RowSegment::plain(format!(
                "{}{}",
                sep,
                " ".repeat(content_w)
            )));
        }
        // else: provider has no window here and no later column — omit (its line
        // ends; padding it would only add trailing spaces).
    }
    segs
}

/// Try every tier best-first and return the first whose every aligned row fits
/// `width`. `None` when no aligned tier fits (caller falls back to independent
/// per-row rendering).
fn build_aligned_block(
    live: &[&ProviderQuota],
    now_epoch: u64,
    width: usize,
) -> Option<Vec<Vec<RowSegment>>> {
    let provider_cols: Vec<Vec<(usize, &QuotaWindow)>> =
        live.iter().map(|p| provider_columns(&p.windows)).collect();
    let block_max_col = provider_cols
        .iter()
        .flat_map(|cs| cs.iter().map(|(c, _)| *c))
        .max()
        .unwrap_or(0);
    // Columns that at least one provider fills — a provider only blank-pads an
    // absent column when another row actually uses it (so the padding is
    // alignment, not a gratuitous gap on a lone/short row).
    let mut column_used = vec![false; block_max_col + 1];
    for cs in &provider_cols {
        for (c, _) in cs {
            column_used[*c] = true;
        }
    }
    let id_w = live
        .iter()
        .map(|p| p.id.chars().count())
        .max()
        .unwrap_or(0)
        .max(6);
    for tier in TIERS {
        let layout = compute_layout(&provider_cols, id_w, block_max_col, *tier, now_epoch);
        let rows: Vec<Vec<RowSegment>> = live
            .iter()
            .zip(provider_cols.iter())
            .map(|(p, cols)| {
                render_aligned_row(
                    cols,
                    p,
                    &layout,
                    block_max_col,
                    &column_used,
                    *tier,
                    now_epoch,
                )
            })
            .collect();
        if rows.iter().all(|r| line_width(r) <= width) {
            return Some(rows);
        }
    }
    None
}

/// Render one live provider on its own: best-fitting tier, with a hard-truncate
/// fallback at absurdly narrow widths. Identical to the single-row contract of
/// [`build_provider_line`], and the per-row shape the aligned block falls back
/// to. A single provider fills exactly the columns it has, so nothing is blanked
/// or padded for an absent neighbour.
fn build_row_independent(p: &ProviderQuota, now_epoch: u64, width: usize) -> Vec<RowSegment> {
    let provider_cols = provider_columns(&p.windows);
    let block_max_col = provider_cols.iter().map(|(c, _)| *c).max().unwrap_or(0);
    let column_used: Vec<bool> = {
        let mut v = vec![false; block_max_col + 1];
        for (c, _) in &provider_cols {
            v[*c] = true;
        }
        v
    };
    let id_w = p.id.chars().count().max(6);
    for tier in TIERS {
        let layout = compute_layout(
            std::slice::from_ref(&provider_cols),
            id_w,
            block_max_col,
            *tier,
            now_epoch,
        );
        let segs = render_aligned_row(
            &provider_cols,
            p,
            &layout,
            block_max_col,
            &column_used,
            *tier,
            now_epoch,
        );
        if line_width(&segs) <= width {
            return segs;
        }
    }
    // Exhausted every tier (absurdly narrow): hard-truncate the cheapest tier.
    let tier = *TIERS.last().unwrap();
    let layout = compute_layout(
        std::slice::from_ref(&provider_cols),
        id_w,
        block_max_col,
        tier,
        now_epoch,
    );
    let fallback = render_aligned_row(
        &provider_cols,
        p,
        &layout,
        block_max_col,
        &column_used,
        tier,
        now_epoch,
    );
    truncate_segments(fallback, width)
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
    fn long_provider_id_keeps_gap_before_first_label() {
        // `copilot` is exactly 7 chars — the id field must still separate it from
        // the first window label instead of running into it ("copilotsession").
        let mut p = claude();
        p.id = "copilot".to_string();
        p.windows.truncate(1);
        let line = render(&build_provider_line(&p, NOW, 110));
        assert!(line.starts_with(" copilot session "), "{line}");
    }

    #[test]
    fn short_provider_id_alignment_unchanged() {
        // Ids up to 6 chars keep the original fixed id column (prefix width 8).
        let mut p = claude();
        p.id = "zai".to_string();
        p.windows.truncate(1);
        let line = render(&build_provider_line(&p, NOW, 110));
        assert!(line.starts_with(" zai    session "), "{line}");
    }

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

    // --- Fault 1: canonical window order (session, week, extras) ---

    /// A provider whose API returns windows in non-canonical source order, with
    /// two unknown extra windows (neither `session` nor `week`, and not on any
    /// hardcoded list). Source order here is: extra, week, session, extra.
    fn scrambled_zai() -> ProviderQuota {
        ProviderQuota {
            id: "zai".to_string(),
            label: "Z.AI".to_string(),
            plan: None,
            windows: vec![
                QuotaWindow {
                    id: "mcp_month".to_string(),
                    label: "MCP month".to_string(),
                    percent_used: 0.0,
                    resets_at: Some("2026-08-24T21:55:00Z".to_string()),
                },
                QuotaWindow {
                    id: "seven_day".to_string(),
                    label: "week".to_string(),
                    percent_used: 51.0,
                    resets_at: Some("2026-08-14T21:55:00Z".to_string()),
                },
                QuotaWindow {
                    id: "five_hour".to_string(),
                    label: "session".to_string(),
                    percent_used: 42.0,
                    resets_at: Some("2026-08-13T14:38:00Z".to_string()),
                },
                QuotaWindow {
                    id: "model:zebra".to_string(),
                    label: "Zebra pool".to_string(),
                    percent_used: 7.0,
                    resets_at: Some("2026-08-24T21:55:00Z".to_string()),
                },
            ],
            status: ProviderStatus::Fresh,
            stale: false,
            error: None,
        }
    }

    /// Char index of the start of each maximal bar-glyph run (a bar). Used to
    /// assert bars line up across rows without hardcoding the whole string.
    fn bar_starts(s: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let mut prev_bar = false;
        for (i, c) in s.chars().enumerate() {
            let is_bar = c == '\u{2588}' || c == '\u{2591}';
            if is_bar && !prev_bar {
                out.push(i);
            }
            prev_bar = is_bar;
        }
        out
    }

    #[test]
    fn canonical_order_in_once_line_when_source_differs() {
        // Source order is MCP month, week, session, zebra — the API returned the
        // monthly extra first (exactly the captain's zai fault). Canonical order
        // must win: session, week, then extras alphabetically (MCP month < zebra).
        let line = once_line(&scrambled_zai(), NOW);
        let s = line.find("session").unwrap();
        let w = line.find("week").unwrap();
        let m = line.find("MCP month").unwrap();
        let z = line.find("zebra").unwrap();
        assert!(s < w, "session before week: {line}");
        assert!(w < m, "week before extras: {line}");
        assert!(m < z, "extras alphabetical (MCP month < zebra): {line}");
        // And the extra that the API returned first is no longer leading.
        assert!(s < m, "session leads, not the API-first MCP month: {line}");
    }

    #[test]
    fn canonical_order_in_tui_row_when_source_differs() {
        let line = render(&build_provider_line(&scrambled_zai(), NOW, 130));
        let s = line.find("session").unwrap();
        let w = line.find("week").unwrap();
        let m = line.find("MCP month").unwrap();
        let z = line.find("zebra").unwrap();
        assert!(
            s < w && w < m && m < z,
            "canonical order in TUI row: {line}"
        );
    }

    #[test]
    fn unknown_extra_window_sorts_as_extra() {
        // A window labelled with a label no provider list knows about still
        // lands after week on its own (rank 2), not first as the API returned.
        let mut p = claude();
        p.windows = vec![
            QuotaWindow {
                id: "model:zebra".to_string(),
                label: "Zebra pool".to_string(),
                percent_used: 7.0,
                resets_at: None,
            },
            QuotaWindow {
                id: "five_hour".to_string(),
                label: "session".to_string(),
                percent_used: 5.0,
                resets_at: None,
            },
        ];
        let line = render(&build_provider_line(&p, NOW, 130));
        assert!(
            line.find("session").unwrap() < line.find("zebra").unwrap(),
            "{line}"
        );
    }

    // --- Fault 2: aligned columns across rows ---

    /// A second live provider with a session/week/extra layout whose extra label
    /// ("MCP month", 9 chars) is wider than claude's ("fable", 5) and whose
    /// resets are shorter, so alignment is non-trivial. Source order canonical.
    fn zai_aligned() -> ProviderQuota {
        ProviderQuota {
            id: "zai".to_string(),
            label: "Z.AI".to_string(),
            plan: None,
            windows: vec![
                QuotaWindow {
                    id: "five_hour".to_string(),
                    label: "session".to_string(),
                    percent_used: 42.0,
                    resets_at: Some("2026-08-13T14:38:00Z".to_string()),
                },
                QuotaWindow {
                    id: "seven_day".to_string(),
                    label: "week".to_string(),
                    percent_used: 51.0,
                    resets_at: Some("2026-08-14T21:55:00Z".to_string()),
                },
                QuotaWindow {
                    id: "mcp_month".to_string(),
                    label: "MCP month".to_string(),
                    percent_used: 0.0,
                    resets_at: Some("2026-08-24T21:55:00Z".to_string()),
                },
            ],
            status: ProviderStatus::Fresh,
            stale: false,
            error: None,
        }
    }

    #[test]
    fn aligned_block_bars_line_up_across_rows() {
        // Two rows whose labels AND reset widths differ. The bars of every
        // window kind must start at the same column down the rows — including
        // the extra column, where "fable" (5) and "MCP month" (9) differ.
        let claude = claude();
        let zai = zai_aligned();
        let providers = vec![&claude, &zai];
        let block = build_quota_rows(&providers, NOW, 140);
        assert_eq!(block.len(), 2, "two rows");
        let a = render(&block[0]);
        let b = render(&block[1]);
        let sa = bar_starts(&a);
        let sb = bar_starts(&b);
        assert_eq!(sa.len(), 3, "claude has 3 bars: {a}");
        assert_eq!(sb.len(), 3, "zai has 3 bars: {b}");
        assert_eq!(
            sa, sb,
            "bars must line up across rows:\n  claude: {a}\n  zai:    {b}"
        );
        for (row, label) in [(&a, "claude"), (&b, "zai")] {
            assert!(
                row.chars().count() <= 140,
                "{label} fits ({})",
                row.chars().count()
            );
        }
    }

    #[test]
    fn aligned_block_lines_up_label_columns_too() {
        // Shared column grid, not just the bars: each shared window's label
        // starts at the same x in every row, and the extra column (whose labels
        // differ: "fable" vs "MCP month") still starts its bar at the same x
        // because the shorter label is padded to the column width.
        let claude = claude();
        let zai = zai_aligned();
        let block = build_quota_rows(&[&claude, &zai], NOW, 140);
        let a = render(&block[0]);
        let b = render(&block[1]);
        for token in ["session", "week"] {
            assert_eq!(
                a.find(token),
                b.find(token),
                "{token} label aligned:\n  {a}\n  {b}"
            );
        }
        assert_eq!(
            bar_starts(&a)[2],
            bar_starts(&b)[2],
            "extra-column bar aligned despite differing labels:\n  {a}\n  {b}"
        );
    }

    #[test]
    fn single_provider_in_block_is_not_padded() {
        // One provider alone must render exactly as the single-row path: no
        // column padding for an absent neighbour.
        let block = build_quota_rows(&[&claude()], NOW, 110);
        assert_eq!(block.len(), 1);
        assert_eq!(
            render(&block[0]),
            render(&build_provider_line(&claude(), NOW, 110)),
            "single-provider block == single-row ladder"
        );
    }

    #[test]
    fn lone_provider_with_only_extras_is_not_leading_padded() {
        // A provider whose windows are all extras (no session/week) must not get
        // blank-padded for the absent session/week columns when it is alone —
        // there is no other row to align against, so no padding is warranted.
        let extras_only = ProviderQuota {
            id: "novel".to_string(),
            label: "Novel".to_string(),
            plan: None,
            windows: vec![
                QuotaWindow {
                    id: "model:alpha".to_string(),
                    label: "Alpha".to_string(),
                    percent_used: 10.0,
                    resets_at: Some("2026-08-17T12:00:00.000000+00:00".to_string()),
                },
                QuotaWindow {
                    id: "model:beta".to_string(),
                    label: "Beta".to_string(),
                    percent_used: 20.0,
                    resets_at: Some("2026-08-17T12:00:00.000000+00:00".to_string()),
                },
            ],
            status: ProviderStatus::Fresh,
            stale: false,
            error: None,
        };
        let block = build_quota_rows(&[&extras_only], NOW, 120);
        assert_eq!(block.len(), 1);
        let line = render(&block[0]);
        // Alone, it renders exactly as the single-row path (no neighbour → no
        // padding for the absent session/week columns).
        assert_eq!(
            line,
            render(&build_provider_line(&extras_only, NOW, 120)),
            "lone extras-only provider == single-row path"
        );
        // Concretely, no blanked-column gap: a padded session/week column would
        // be ~20+ spaces; legitimate field/separator spacing never reaches 8.
        assert!(!line.contains("        "), "no blanked-column gap: {line}");
        // Extras land in alphabetical order.
        assert!(
            line.find("alpha").unwrap() < line.find("beta").unwrap(),
            "{line}"
        );
    }

    #[test]
    fn aligned_block_degrades_without_overflow_when_narrow() {
        // At a width where no aligned tier fits, alignment is dropped and each
        // row degrades independently (hard-truncate at absurd widths) — never
        // wraps or overflows.
        let claude = claude();
        let zai = zai_aligned();
        let providers = vec![&claude, &zai];
        let block = build_quota_rows(&providers, NOW, 20);
        assert_eq!(block.len(), 2);
        for (i, row) in block.iter().enumerate() {
            let line = render(row);
            assert!(
                line.chars().count() <= 20,
                "row {i} fits ({}): {line}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn dim_provider_among_live_keeps_live_rows_aligned() {
        // An explicit selection may include a non-live provider; it renders its
        // dim line independently while the live rows still align their bars.
        let claude = claude();
        let zai = zai_aligned();
        let copilot = ProviderQuota {
            id: "copilot".to_string(),
            label: "GitHub Copilot".to_string(),
            plan: None,
            windows: vec![],
            status: ProviderStatus::AuthRequired,
            stale: false,
            error: Some("GitHub Copilot sign-in required".to_string()),
        };
        let providers = vec![&copilot, &claude, &zai];
        let block = build_quota_rows(&providers, NOW, 140);
        assert_eq!(block.len(), 3);
        assert_eq!(render(&block[0]), " copilot sign-in required");
        let a = render(&block[1]);
        let b = render(&block[2]);
        assert_eq!(bar_starts(&a), bar_starts(&b), "live rows still aligned");
    }
}
