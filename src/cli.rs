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
}
