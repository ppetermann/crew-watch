//! Command-line interface.

use std::path::PathBuf;

use clap::Parser;

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
}
