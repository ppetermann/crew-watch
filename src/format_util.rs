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
}
