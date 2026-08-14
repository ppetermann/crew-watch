//! Pure column geometry and value fitting for the TUI agent table.
//!
//! ### Why this module exists
//!
//! The numeric columns (PID, ELAPSED, CPU%, MEM) are right-aligned so their
//! magnitudes stack. ratatui renders an over-width right-aligned line by
//! skipping from the **left** (`Line::render_with_alignment`), which drops the
//! magnitude digits and leaves a plausible wrong number: `848.6MiB` in a
//! 6-wide cell would read `8.6MiB`. Losing precision is fine, losing magnitude
//! is not.
//!
//! The fix is to never hand ratatui an over-width right-aligned cell:
//! [`compressed_col_widths`] decides what each fixed column actually gets at
//! the current terminal width, and those same widths are used both as the
//! table's `Length` constraints and as each cell's fitting budget. Because the
//! constraints already fit the available space, ratatui never shrinks a column
//! itself, so alignment never triggers left-truncation.
//!
//! ### Fitting rule
//!
//! Every `fit_*` helper shortens by unit and precision first (`848.6MiB` →
//! `849MiB` → `849M`), and only cuts characters — always keeping the *leading*
//! ones — when no shorter rendering exists. No ellipsis: in a numeric cell it
//! would eat a digit slot. Each helper's result is guaranteed to be at most
//! `width` characters.

use crate::format_util::{format_duration, format_kib, format_percent};

/// Fixed widths of every agent-table column before the flexible TASK column,
/// at a terminal wide enough to hold them (RUNTIME, MODEL, PID, ELAPSED,
/// CPU%, MEM).
pub const AGENT_FIXED_COL_WIDTHS: [u16; 6] = [10, 14, 8, 10, 9, 12];
/// Blank columns ratatui inserts between two adjacent table columns.
pub const AGENT_COL_SPACING: u16 = 1;
/// TASK is never squeezed out entirely: it keeps at least this much even when
/// the fixed columns have to give way.
const MIN_TASK_WIDTH: u16 = 1;

/// Total blank space between the seven columns.
fn spacing_total() -> u16 {
    AGENT_COL_SPACING * (AGENT_FIXED_COL_WIDTHS.len() as u16)
}

/// Widths the six fixed columns get inside a table of `area_width` outer
/// columns (borders included).
///
/// Returns [`AGENT_FIXED_COL_WIDTHS`] unchanged whenever they fit alongside
/// the spacing and a minimal TASK column — so nothing about wide-terminal
/// rendering changes. Below that, every column shrinks proportionally (floor,
/// never below one column), leaving the remainder to TASK.
pub fn compressed_col_widths(area_width: u16) -> [u16; 6] {
    let nominal_total: u16 = AGENT_FIXED_COL_WIDTHS.iter().sum();
    let inner = area_width.saturating_sub(2);
    let budget = inner
        .saturating_sub(spacing_total())
        .saturating_sub(MIN_TASK_WIDTH);
    if budget >= nominal_total {
        return AGENT_FIXED_COL_WIDTHS;
    }

    let mut out = [1u16; 6];
    let mut used = 0u16;
    for (slot, nominal) in out.iter_mut().zip(AGENT_FIXED_COL_WIDTHS) {
        let scaled = (u32::from(nominal) * u32::from(budget)) / u32::from(nominal_total);
        *slot = (scaled as u16).max(1);
        used += *slot;
    }
    // The min-one floor can push the total past the budget on an absurdly
    // narrow terminal; give the overshoot back from the widest column first.
    while used > budget {
        let Some(widest) = widest_shrinkable(&out) else {
            break;
        };
        out[widest] -= 1;
        used -= 1;
    }
    out
}

/// Index of the widest column that can still give up a column, `None` when
/// every column is already at the one-column floor.
fn widest_shrinkable(widths: &[u16; 6]) -> Option<usize> {
    widths
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 1)
        .max_by_key(|(_, w)| **w)
        .map(|(i, _)| i)
}

/// Width the flexible TASK column gets, given the fixed widths in use.
pub fn task_width(area_width: u16, fixed: &[u16; 6]) -> usize {
    let fixed_total: u16 = fixed.iter().sum();
    area_width
        .saturating_sub(2)
        .saturating_sub(fixed_total)
        .saturating_sub(spacing_total())
        .max(MIN_TASK_WIDTH) as usize
}

/// Fit a column header into `width`, keeping its leading characters
/// (`ELAPSED` at 5 → `ELAPS`), so a header still names its column.
pub fn fit_header(header: &str, width: usize) -> String {
    keep_leading(header, width)
}

/// Fit a pid into `width`. A pid has no unit or precision to trade away, so it
/// can only lose trailing digits — the leading ones stay.
pub fn fit_pid(pid: i32, width: usize) -> String {
    keep_leading(&pid.to_string(), width)
}

/// Fit a CPU percentage into `width`: `31.8%` → `32%` → leading digits.
pub fn fit_cpu(pct: f64, width: usize) -> String {
    let full = format_percent(pct);
    if full.chars().count() <= width {
        return full;
    }
    let coarse = format!("{:.0}%", pct);
    if coarse.chars().count() <= width {
        return coarse;
    }
    keep_leading(&coarse, width)
}

/// Fit an elapsed duration into `width`: `38:23:40` → `38:23` → `38h` →
/// leading digits. Sub-hour durations drop to `{m}m` the same way.
pub fn fit_elapsed(secs: u64, width: usize) -> String {
    let full = format_duration(secs);
    if full.chars().count() <= width {
        return full;
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        let hm = format!("{}:{:02}", h, m);
        if hm.chars().count() <= width {
            return hm;
        }
        let hours = format!("{}h", h);
        if hours.chars().count() <= width {
            return hours;
        }
        return keep_leading(&hours, width);
    }
    if m > 0 {
        let mins = format!("{}m", m);
        if mins.chars().count() <= width {
            return mins;
        }
        return keep_leading(&mins, width);
    }
    keep_leading(&full, width)
}

/// Fit a resident-set size given in KiB into `width`: `848.6MiB` → `849MiB` →
/// `849M` → leading digits.
pub fn fit_mem(rss_kib: u64, width: usize) -> String {
    let full = format_kib(rss_kib);
    if full.chars().count() <= width {
        return full;
    }
    let (value, unit, short) = scaled_mem(rss_kib);
    let no_decimal = format!("{:.0}{}", value, unit);
    if no_decimal.chars().count() <= width {
        return no_decimal;
    }
    let short_unit = format!("{:.0}{}", value, short);
    if short_unit.chars().count() <= width {
        return short_unit;
    }
    keep_leading(&short_unit, width)
}

/// Split a KiB value into the same magnitude and unit [`format_kib`] picks,
/// plus the one-letter form of that unit.
fn scaled_mem(kib: u64) -> (f64, &'static str, &'static str) {
    let v = kib as f64;
    if v >= 1_073_741_824.0 {
        (v / 1_073_741_824.0, "TiB", "T")
    } else if v >= 1_048_576.0 {
        (v / 1_048_576.0, "GiB", "G")
    } else if v >= 1024.0 {
        (v / 1024.0, "MiB", "M")
    } else {
        (v, "KiB", "K")
    }
}

/// Keep the first `width` characters of `s` (char-boundary safe).
fn keep_leading(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Narrowest terminal that still holds every nominal fixed column.
    const NOMINAL_MIN_WIDTH: u16 = 2 + 63 + 6 + 1;

    #[test]
    fn wide_terminal_keeps_the_nominal_widths() {
        assert_eq!(compressed_col_widths(200), AGENT_FIXED_COL_WIDTHS);
        assert_eq!(compressed_col_widths(120), AGENT_FIXED_COL_WIDTHS);
        assert_eq!(compressed_col_widths(80), AGENT_FIXED_COL_WIDTHS);
        // Exactly at the boundary the nominal widths still fit.
        assert_eq!(
            compressed_col_widths(NOMINAL_MIN_WIDTH),
            AGENT_FIXED_COL_WIDTHS
        );
        // One column narrower and the shrink kicks in.
        assert_ne!(
            compressed_col_widths(NOMINAL_MIN_WIDTH - 1),
            AGENT_FIXED_COL_WIDTHS
        );
    }

    #[test]
    fn narrow_terminal_shrinks_within_budget_and_never_below_one() {
        for area_width in 1u16..=NOMINAL_MIN_WIDTH {
            let widths = compressed_col_widths(area_width);
            let total: u16 = widths.iter().sum();
            assert!(
                widths.iter().all(|w| *w >= 1),
                "width {area_width} produced a zero-width column: {widths:?}"
            );
            let budget = area_width
                .saturating_sub(2)
                .saturating_sub(6)
                .saturating_sub(1);
            // The six one-column floors are the hard minimum; above that the
            // fixed columns always stay inside the budget.
            assert!(
                total <= budget.max(6),
                "width {area_width}: {widths:?} sums to {total}, budget {budget}"
            );
        }
    }

    #[test]
    fn task_column_always_keeps_at_least_one_column() {
        for area_width in 0u16..=200 {
            let widths = compressed_col_widths(area_width);
            assert!(task_width(area_width, &widths) >= 1);
        }
    }

    #[test]
    fn table_never_overflows_the_terminal_width() {
        // Everything the table lays out — fixed columns, spacing, TASK — has
        // to fit inside the block's inner width, at every width where the
        // columns can hold their one-column floor.
        for area_width in 15u16..=200 {
            let widths = compressed_col_widths(area_width);
            let total: u16 = widths.iter().sum();
            let laid_out = total + 6 + task_width(area_width, &widths) as u16;
            assert_eq!(
                laid_out,
                area_width - 2,
                "width {area_width} lays out {laid_out} inside {}",
                area_width - 2
            );
        }
    }

    #[test]
    fn mem_ladder_trades_precision_then_unit_then_digits() {
        let rss = 868_966; // 848.6 MiB
        assert_eq!(fit_mem(rss, 12), "848.6MiB");
        assert_eq!(fit_mem(rss, 8), "848.6MiB");
        // No room for the decimal: drop it before touching a digit.
        assert_eq!(fit_mem(rss, 7), "849MiB");
        assert_eq!(fit_mem(rss, 6), "849MiB");
        // No room for the full unit: shorten the unit, keep every digit.
        assert_eq!(fit_mem(rss, 5), "849M");
        assert_eq!(fit_mem(rss, 4), "849M");
        // Below that, digits go from the right — magnitude survives.
        assert_eq!(fit_mem(rss, 3), "849");
        assert_eq!(fit_mem(rss, 1), "8");
        assert_eq!(fit_mem(rss, 0), "");
    }

    #[test]
    fn mem_ladder_covers_every_unit() {
        assert_eq!(fit_mem(12_884_901, 8), "12.3GiB");
        assert_eq!(fit_mem(12_884_901, 5), "12GiB");
        assert_eq!(fit_mem(12_884_901, 4), "12G");
        assert_eq!(fit_mem(12_884_901, 3), "12G");
        assert_eq!(fit_mem(512, 6), "512KiB");
        assert_eq!(fit_mem(512, 4), "512K");
        assert_eq!(fit_mem(1_073_741_824, 6), "1.0TiB");
        assert_eq!(fit_mem(1_073_741_824, 4), "1TiB");
        assert_eq!(fit_mem(1_073_741_824, 2), "1T");
    }

    #[test]
    fn elapsed_ladder_keeps_the_hours() {
        let secs = 38 * 3600 + 23 * 60 + 40; // 38:23:40
        assert_eq!(fit_elapsed(secs, 10), "38:23:40");
        assert_eq!(fit_elapsed(secs, 8), "38:23:40");
        // Seconds go before any digit of the hours does.
        assert_eq!(fit_elapsed(secs, 7), "38:23");
        assert_eq!(fit_elapsed(secs, 5), "38:23");
        // Then the minutes, as a compact hour count.
        assert_eq!(fit_elapsed(secs, 4), "38h");
        assert_eq!(fit_elapsed(secs, 3), "38h");
        assert_eq!(fit_elapsed(secs, 2), "38");
        assert_eq!(fit_elapsed(secs, 1), "3");
    }

    #[test]
    fn elapsed_ladder_handles_sub_hour_and_sub_minute() {
        assert_eq!(fit_elapsed(125, 10), "2:05");
        assert_eq!(fit_elapsed(125, 4), "2:05");
        assert_eq!(fit_elapsed(125, 3), "2m");
        assert_eq!(fit_elapsed(125, 1), "2");
        assert_eq!(fit_elapsed(5, 4), "5s");
        assert_eq!(fit_elapsed(5, 1), "5");
    }

    #[test]
    fn cpu_ladder_drops_the_decimal_before_a_digit() {
        assert_eq!(fit_cpu(131.8, 9), "131.8%");
        assert_eq!(fit_cpu(131.8, 6), "131.8%");
        assert_eq!(fit_cpu(131.8, 5), "132%");
        assert_eq!(fit_cpu(131.8, 4), "132%");
        assert_eq!(fit_cpu(131.8, 3), "132");
        assert_eq!(fit_cpu(131.8, 2), "13");
        assert_eq!(fit_cpu(2.0, 4), "2.0%");
        assert_eq!(fit_cpu(2.0, 3), "2%");
    }

    #[test]
    fn pid_and_header_keep_their_leading_characters() {
        assert_eq!(fit_pid(827_115, 8), "827115");
        assert_eq!(fit_pid(827_115, 4), "8271");
        assert_eq!(fit_pid(827_115, 0), "");
        assert_eq!(fit_header("ELAPSED", 10), "ELAPSED");
        assert_eq!(fit_header("ELAPSED", 5), "ELAPS");
        assert_eq!(fit_header("MEM", 1), "M");
    }

    #[test]
    fn every_fitted_cell_stays_inside_its_budget() {
        // The invariant the right-alignment depends on: nothing handed to
        // ratatui is ever wider than the column it goes into, at any terminal
        // width, so alignment never truncates from the left.
        let pids = [1, 42, 827_115, i32::MAX];
        let elapsed = [0, 5, 125, 3725, 38 * 3600 + 23 * 60 + 40, 900 * 3600];
        let cpus = [0.0, 2.0, 31.8, 131.8, 1600.0];
        let mems = [0, 512, 868_966, 12_884_901, 3_221_225_472];
        for area_width in 1u16..=200 {
            let widths = compressed_col_widths(area_width);
            let (pid_w, elapsed_w, cpu_w, mem_w) = (
                widths[2] as usize,
                widths[3] as usize,
                widths[4] as usize,
                widths[5] as usize,
            );
            for pid in pids {
                assert!(fit_pid(pid, pid_w).chars().count() <= pid_w);
            }
            for secs in elapsed {
                assert!(fit_elapsed(secs, elapsed_w).chars().count() <= elapsed_w);
            }
            for pct in cpus {
                assert!(fit_cpu(pct, cpu_w).chars().count() <= cpu_w);
            }
            for kib in mems {
                assert!(fit_mem(kib, mem_w).chars().count() <= mem_w);
            }
            for (header, w) in [
                ("PID", pid_w),
                ("ELAPSED", elapsed_w),
                ("CPU%", cpu_w),
                ("MEM", mem_w),
            ] {
                assert!(fit_header(header, w).chars().count() <= w);
            }
        }
    }
}
