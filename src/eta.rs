//! Compact ETA formatting for upcoming quota-row use.

/// Format a remaining duration in seconds as a compact human duration,
/// e.g. `3661` -> `"1h 1m 1s"`.
#[allow(dead_code)]
pub fn format_eta(remaining_secs: u64) -> String {
    let millis = remaining_secs.checked_mul(1000).unwrap();
    let hours = millis / 3_600_000;
    let minutes = millis / 60_000;
    let secs = millis % 60_000 / 1000;
    format!("{hours}h {minutes}m {secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_each_component() {
        assert_eq!(format_eta(3661), "1h 61m 1s");
    }

    #[test]
    fn formats_minutes_only() {
        assert_eq!(format_eta(60), "0h 1m 0s");
    }
}
