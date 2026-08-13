//! Small formatting helpers for memory, durations, and percentages.
//! Pure functions so they can be unit-tested directly.

/// Format a value given in KiB as a compact human-readable size.
pub fn format_kib(kib: u64) -> String {
    let v = kib as f64;
    if v >= 1_073_741_824.0 {
        format!("{:.1}TiB", v / 1_073_741_824.0)
    } else if v >= 1_048_576.0 {
        format!("{:.1}GiB", v / 1_048_576.0)
    } else if v >= 1024.0 {
        format!("{:.1}MiB", v / 1024.0)
    } else {
        format!("{:.0}KiB", v)
    }
}

/// Format an elapsed duration in seconds as htop-style `H:MM:SS` (or `M:SS`,
/// or `Ns` for very short durations).
pub fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else if m > 0 {
        format!("{}:{:02}", m, s)
    } else {
        format!("{}s", s)
    }
}

/// Format a system uptime (seconds), folding days for readability.
pub fn format_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if days > 0 {
        format!("{}d {}:{:02}:{:02}", days, h, m, s)
    } else {
        format_duration(secs)
    }
}

/// Format a percentage with one decimal place.
pub fn format_percent(pct: f64) -> String {
    format!("{:.1}%", pct)
}

/// Full-block glyph used by every bar in crew-watch (CPU, memory, swap, quota).
pub const FILL_BLOCK: char = '\u{2588}'; // full block
/// Light-shade glyph pairing [`FILL_BLOCK`] for the empty portion of a bar.
pub const SHADE_BLOCK: char = '\u{2591}'; // light shade

/// Build a fixed-width usage bar in crew-watch's `█░` style. `pct` is clamped to
/// 0–100; the filled count is `round(pct/100 * width)`. Shared by the system
/// meters (CPU core cells, memory, swap) and the quota row so every bar in the
/// UI uses identical glyphs.
pub fn make_bar(pct: f64, width: usize) -> String {
    let width = width.max(1);
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push(FILL_BLOCK);
    }
    for _ in filled..width {
        s.push(SHADE_BLOCK);
    }
    s
}

/// Compact "time until reset" label for a quota window, given seconds remaining.
/// `≥1d → "{d}d{h}h"`, `≥1h → "{h}h{m}m"`, else `"{m}m"` (so `9m`, `1h45m`,
/// `3d22h`). Minutes are dropped once days are shown (the day/hour pair is
/// enough resolution for a reset countdown).
pub fn format_reset(secs: u64) -> String {
    let days = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if days >= 1 {
        format!("{}d{}h", days, h)
    } else if h >= 1 {
        format!("{}h{}m", h, m)
    } else {
        format!("{}m", m)
    }
}

/// Compact age label for the staleness suffix, scaling to hours once a cadence
/// of repeated failures pushes the age past an hour (e.g. at the 5-minute quota
/// cadence a few stacked misses). `<60m → "{m}m"`, else `"{h}h{m}m"`.
pub fn format_age_compact(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h >= 1 {
        format!("{}h{}m", h, m)
    } else {
        format!("{}m", m.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_kib() {
        assert_eq!(format_kib(0), "0KiB");
        assert_eq!(format_kib(512), "512KiB");
        assert_eq!(format_kib(2048), "2.0MiB");
        assert_eq!(format_kib(1_048_576), "1.0GiB");
        assert_eq!(format_kib(12_884_901), "12.3GiB");
        assert_eq!(format_kib(1_073_741_824), "1.0TiB");
    }

    #[test]
    fn fmt_duration() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(5), "5s");
        assert_eq!(format_duration(65), "1:05");
        assert_eq!(format_duration(3725), "1:02:05");
    }

    #[test]
    fn fmt_uptime() {
        assert_eq!(format_uptime(125), "2:05");
        assert_eq!(
            format_uptime(3 * 86_400 + 5 * 3600 + 20 * 60 + 11),
            "3d 5:20:11"
        );
    }

    #[test]
    fn fmt_percent() {
        assert_eq!(format_percent(12.34), "12.3%");
        assert_eq!(format_percent(0.0), "0.0%");
    }

    #[test]
    fn fmt_make_bar() {
        // Full / empty extremes.
        assert_eq!(make_bar(0.0, 4), "░░░░");
        assert_eq!(make_bar(100.0, 4), "████");
        // 50% of 4 rounds to 2 filled.
        assert_eq!(make_bar(50.0, 4), "██░░");
        // Out-of-range clamps for fill only.
        assert_eq!(make_bar(150.0, 4), "████");
        assert_eq!(make_bar(-5.0, 4), "░░░░");
        // width 0 collapses to 1 (never panics).
        assert_eq!(make_bar(50.0, 0).chars().count(), 1);
    }

    #[test]
    fn fmt_reset() {
        assert_eq!(format_reset(0), "0m");
        assert_eq!(format_reset(540), "9m");
        assert_eq!(format_reset(6300), "1h45m");
        assert_eq!(format_reset(338_700), "3d22h"); // 3d 22h 5m -> minutes dropped
                                                    // Day boundary exactly.
        assert_eq!(format_reset(86_400), "1d0h");
    }

    #[test]
    fn fmt_age_compact() {
        assert_eq!(format_age_compact(30), "1m"); // sub-minute floors to 1m
        assert_eq!(format_age_compact(900), "15m");
        assert_eq!(format_age_compact(5400), "1h30m");
    }
}
