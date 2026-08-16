//! Compact ETA formatting for upcoming quota-row use.

/// Format a remaining duration in seconds as a compact human duration,
/// e.g. `3661` -> `"1h 1m 1s"`.
#[allow(dead_code)]
pub fn format_eta(remaining_secs: u64) -> String {
    let hours = remaining_secs / 3600;
    let minutes = remaining_secs % 3600 / 60;
    let secs = remaining_secs % 60;
    format!("{hours}h {minutes}m {secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_each_component() {
        assert_eq!(format_eta(3661), "1h 1m 1s");
    }

    #[test]
    fn formats_over_an_hour() {
        assert_eq!(format_eta(7322), "2h 2m 2s");
    }

    #[test]
    fn formats_minutes_only() {
        assert_eq!(format_eta(60), "0h 1m 0s");
    }

    #[test]
    fn formats_sub_minute() {
        assert_eq!(format_eta(5), "0h 0m 5s");
    }

    #[test]
    fn large_input_does_not_panic() {
        assert_eq!(format_eta(u64::MAX), "5124095576030431h 0m 15s");
    }
}
