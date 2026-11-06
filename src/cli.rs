//! Command-line interface.

use std::path::PathBuf;

use clap::Parser;

use crate::openrouter::UsageWindow;

/// `--openrouter-harness`: a runtime id from the detection table, or `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessChoice {
    Runtime(&'static str),
    Off,
}

fn parse_harness(s: &str) -> Result<HarnessChoice, String> {
    if s == "none" {
        return Ok(HarnessChoice::Off);
    }
    crate::detect::AGENT_KINDS
        .iter()
        .find(|k| k.id == s)
        .map(|k| HarnessChoice::Runtime(k.id))
        .ok_or_else(|| {
            let ids: Vec<&str> = crate::detect::AGENT_KINDS.iter().map(|k| k.id).collect();
            format!("expected one of {}, none", ids.join(", "))
        })
}

/// TUI fleet monitor: htop-style system overview plus a per-agent view of
/// running AI agent runtimes and their whole process-subtree CPU/MEM cost.
#[derive(Parser, Debug)]
#[command(
    name = "crew-watch",
    version,
    about = "htop-style system overview + agent-centric fleet monitor"
)]
pub struct Cli {
    /// Refresh interval in seconds.
    #[arg(long, default_value_t = 2.0)]
    pub interval: f64,

    /// Firstmate home directory (reads state/*.meta). Defaults to
    /// $CREW_WATCH_FM_HOME, or ~/agents/firstmate.
    #[arg(long, env = "CREW_WATCH_FM_HOME")]
    pub fm_home: Option<PathBuf>,

    /// Non-interactive: collect two samples ~1s apart (so CPU% is real), print
    /// the system summary and detected sessions to stdout, and exit. Useful for
    /// scripting and for verifying detection outside a TTY.
    #[arg(long, default_value_t = false)]
    pub once: bool,

    /// Disable the quota row and the `quota-axi` background fetch entirely.
    #[arg(long, default_value_t = false)]
    pub no_quota: bool,

    /// Seconds between `quota-axi` refreshes. Defaults to 600 (10 minutes) and is
    /// clamped to 60..=3600. The 60s floor is **not arbitrary**: quota tooling
    /// rate-limits under polling every couple of minutes, so polling faster than
    /// once a minute is treated as a misconfiguration and refused. The fetch
    /// itself still runs off the `/proc` tick path on a background thread and is
    /// bounded by a 10s kill-timeout regardless of this value.
    #[arg(long, default_value_t = 600.0)]
    pub quota_interval: f64,

    /// Window of the OpenRouter line's spend per model, with a management key
    /// in $CREW_WATCH_OPENROUTER_KEY (never a flag: arguments are visible in
    /// the process list): 1h, 1d or 1w.
    #[arg(
        long,
        env = "CREW_WATCH_OPENROUTER_WINDOW",
        default_value = "1d",
        value_parser = |s: &str| s.parse::<UsageWindow>()
    )]
    pub openrouter_window: UsageWindow,

    /// OpenRouter key name the management key's line is about. Default: the
    /// account's only enabled key, else all keys together.
    #[arg(long, env = "CREW_WATCH_OPENROUTER_KEY_NAME")]
    pub openrouter_key_name: Option<String>,

    /// Without a management key, the harness whose own OpenRouter key the
    /// line reads: a runtime id (claude, opencode, codex, grok, kimi, muse,
    /// pi) or `none` to turn this off. Default: the harness of the session
    /// running in the firstmate home.
    #[arg(long, env = "CREW_WATCH_OPENROUTER_HARNESS", value_parser = parse_harness)]
    pub openrouter_harness: Option<HarnessChoice>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_choice_accepts_detected_runtimes_and_none() {
        assert_eq!(parse_harness("pi"), Ok(HarnessChoice::Runtime("pi")));
        assert_eq!(parse_harness("none"), Ok(HarnessChoice::Off));
        let err = parse_harness("vim").unwrap_err();
        assert!(err.contains("claude") && err.contains("none"), "{err}");
    }

    #[test]
    fn openrouter_flags_parse_and_refuse_unknown_values() {
        let cli = Cli::try_parse_from(["crew-watch", "--openrouter-window", "1w"]).unwrap();
        assert_eq!(cli.openrouter_window, UsageWindow::Week);
        assert!(Cli::try_parse_from(["crew-watch", "--openrouter-window", "2d"]).is_err());
        assert!(Cli::try_parse_from(["crew-watch", "--openrouter-harness", "vim"]).is_err());
    }
}
