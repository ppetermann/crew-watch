//! crew-watch entry point: argument parsing, terminal setup/teardown, and the
//! render/tick event loop.

mod app;
mod cli;
mod config;
mod detect;
mod format_util;
mod meta;
mod model;
mod procfs;
mod project;
mod quota;
mod quota_dialog;
mod quota_row;
mod taskinfo;
mod titles;
mod ui;

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::App;
use crate::cli::Cli;
use crate::quota::{spawn_poller, QuotaFetch};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let interval_secs = if cli.interval.is_finite() {
        cli.interval.clamp(0.1, 3600.0)
    } else {
        2.0
    };
    let interval = Duration::from_secs_f64(interval_secs);
    let fm_home = resolve_fm_home(cli.fm_home);
    let mut app = App::new(interval, fm_home);

    // --- quota configuration (clamped cadence; see cli.rs for the floor) ---
    app.quota_enabled = !cli.no_quota;
    app.quota_interval = resolve_quota_interval(cli.quota_interval);
    if let Some(path) = config::config_path() {
        let cfg = config::load(&path);
        app.quota_selected = cfg.quota_providers;
    }
    // No config path (no $HOME) ⇒ auto mode, session-only; a notice surfaces if
    // the user later tries to persist a selection from the dialog.

    if cli.once {
        run_once(&mut app);
        return Ok(());
    }

    // Spawn the background poller (never on the /proc tick path).
    if app.quota_enabled {
        let (tx, rx) = channel::<QuotaFetch>();
        app.quota_rx = Some(rx);
        spawn_poller(app.quota_interval, tx);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Restore the terminal even on panic so the user is never left in raw mode.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    app.tick();
    let result = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result?;
    Ok(())
}

/// `--once`: two samples ~1s apart (so CPU% is real), with the quota fetch
/// running concurrently on a thread so its ~0.8s wall overlaps the sampling
/// sleep. Then print and exit.
fn run_once(app: &mut App) {
    let quota_handle = if app.quota_enabled {
        Some(std::thread::spawn(|| {
            crate::quota::fetch_once(Duration::from_secs(10))
        }))
    } else {
        None
    };
    app.tick();
    std::thread::sleep(Duration::from_millis(1000));
    app.tick();
    if let Some(h) = quota_handle {
        if let Ok(fetch) = h.join() {
            match fetch {
                QuotaFetch::Report(r) => app.quota.report = Some(r),
                QuotaFetch::Failed(e) => app.quota.last_error = Some(e),
            }
        }
    }
    print_once(app);
}

/// Render + input/tick loop. Polls with a timeout of the remaining time until
/// the next refresh, so input stays responsive while /proc is read at most once
/// per interval.
fn run<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
    let mut last_tick = Instant::now();
    loop {
        app.drain_quota();
        terminal.draw(|f| ui::draw(f, app))?;

        let timeout = app.interval.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(k) = event::read()? {
                // Ctrl-C is the documented always-available escape hatch: it quits
                // even with the dialog open (where Esc/q close the dialog instead).
                if is_ctrl_c(&k) {
                    return Ok(());
                }
                if app.dialog.is_some() {
                    handle_dialog_key(app, k);
                } else {
                    if is_quit(&k) {
                        return Ok(());
                    }
                    if matches!(k.code, KeyCode::Char('p')) && app.quota_enabled {
                        open_dialog(app);
                    }
                    // Any non-quit key clears the transient notice.
                    app.notice = None;
                }
            }
        }
        if last_tick.elapsed() >= app.interval {
            app.tick();
            last_tick = Instant::now();
        }
    }
}

fn is_quit(k: &KeyEvent) -> bool {
    match k.code {
        KeyCode::Char('q') | KeyCode::Esc => true,
        _ => is_ctrl_c(k),
    }
}

fn is_ctrl_c(k: &KeyEvent) -> bool {
    matches!(k.code, KeyCode::Char('c')) && k.modifiers.contains(KeyModifiers::CONTROL)
}

/// Open the provider-selection dialog. The item list is built from the latest
/// report (live + failing providers) plus any stored ids no longer reported, so
/// the list always reflects what quota-axi actually reports — never hardcoded.
fn open_dialog(app: &mut App) {
    let report = app.quota.report.as_ref();
    let stored = app.quota_selected.as_deref();
    let auto = quota_dialog::auto_ids(report);
    app.dialog = Some(quota_dialog::open(report, stored, &auto));
}

/// Route a key to the open dialog. `Save` persists the selection (best-effort;
/// failure surfaces as a notice, the in-memory selection still applies for the
/// session), `Cancel` discards. `Esc` closes the dialog without quitting.
fn handle_dialog_key(app: &mut App, k: KeyEvent) {
    use quota_dialog::Outcome;
    let Some(dialog) = app.dialog.as_mut() else {
        return;
    };
    match dialog.handle_key(k.code) {
        Outcome::Save(ids) => {
            match config::config_path() {
                Some(path) => {
                    if let Err(e) = config::save_quota_providers(&path, &ids) {
                        app.notice = Some(format!("config save failed: {e}"));
                    }
                }
                None => app.notice = Some("selection not persisted (no $HOME)".to_string()),
            }
            app.quota_selected = Some(ids);
            app.dialog = None;
        }
        Outcome::Cancel => {
            app.dialog = None;
        }
        Outcome::Pending => {}
    }
}

/// Effective quota poll cadence for `--quota-interval`, clamped to 60..=3600s
/// (a non-finite argument falls back to the 300s default). The 60s floor is not
/// arbitrary — polling the quota tool faster than once a minute gets the account
/// rate-limited — so an under-floor value is raised, never honoured.
fn resolve_quota_interval(arg_secs: f64) -> Duration {
    let secs = if arg_secs.is_finite() {
        arg_secs.clamp(60.0, 3600.0)
    } else {
        300.0
    };
    Duration::from_secs_f64(secs)
}

fn resolve_fm_home(arg: Option<PathBuf>) -> PathBuf {
    if let Some(p) = arg {
        return p;
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join("agents").join("firstmate");
    }
    PathBuf::from("agents/firstmate")
}

/// Fixed-width prefix of an agent row in `--once` output: every column before
/// TASK plus the separating spaces.
const ONCE_TASK_PREFIX_WIDTH: usize = 69;

/// Non-interactive text dump of one snapshot (the `--once` path).
fn print_once(app: &App) {
    use crate::format_util::{format_duration, format_kib, format_percent, format_uptime};
    use crate::procfs::CpuLine;
    use crate::taskinfo::fit_task_line;
    use std::io::IsTerminal;

    let Some(snap) = app.snapshot() else {
        println!("no snapshot");
        return;
    };
    let cores: Vec<&CpuLine> = snap.cpus.iter().filter(|c| !c.is_aggregate()).collect();
    let mem_used = snap
        .mem
        .mem_total_kib
        .saturating_sub(snap.mem.mem_avail_kib);
    let swap_used = snap
        .mem
        .swap_total_kib
        .saturating_sub(snap.mem.swap_free_kib);
    println!(
        "cores={} mem={}/{} swap={}/{} tasks={} load={:.2} {:.2} {:.2} up={}",
        cores.len(),
        format_kib(mem_used),
        format_kib(snap.mem.mem_total_kib),
        format_kib(swap_used),
        format_kib(snap.mem.swap_total_kib),
        snap.load.total,
        snap.load.one,
        snap.load.five,
        snap.load.fifteen,
        format_uptime(snap.uptime.secs.max(0.0) as u64)
    );

    if app.sessions.is_empty() {
        println!("agents: (none detected)");
    } else {
        println!(
            "{:<10} {:<14} {:>7} {:>10} {:>9} {:>12}  TASK",
            "RUNTIME", "MODEL", "PID", "ELAPSED", "CPU%", "MEM"
        );
        // On a real terminal, fit the TASK column to the remaining width (id
        // dropped first, then ellipsis); when piped/redirected, print the full
        // line so scripts always see the task id. crossterm's size() consults
        // /dev/tty and would succeed even with stdout redirected, hence the
        // explicit is_terminal gate.
        let task_width = if io::stdout().is_terminal() {
            crossterm::terminal::size()
                .ok()
                .map(|(w, _)| (w as usize).saturating_sub(ONCE_TASK_PREFIX_WIDTH).max(1))
        } else {
            None
        };
        for s in &app.sessions {
            let model = if s.model.is_empty() {
                "-".to_string()
            } else {
                s.model.clone()
            };
            let task = match task_width {
                Some(w) => fit_task_line(&s.task, w),
                None => s.task.clone(),
            };
            println!(
                "{:<10} {:<14.14} {:>7} {:>10} {:>9} {:>12}  {}",
                s.kind.display,
                model,
                s.pid,
                format_duration(s.elapsed_secs),
                format_percent(s.cpu_percent),
                format_kib(s.rss_kib),
                task
            );
        }
    }

    print_once_quota(app);
}

/// Print the quota lines for `--once`, after the agents table. Bar-free, one
/// line per effective provider in report order; a fetch-level failure with an
/// explicit selection prints a single greppable `quota: unavailable (...)` line.
/// Auto mode with no provider reporting windows (or `--no-quota`) prints nothing.
fn print_once_quota(app: &App) {
    use std::time::{SystemTime, UNIX_EPOCH};
    if !app.quota_enabled {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match app.effective_selection() {
        crate::app::QuotaSelection::Auto => {
            if let Some(r) = &app.quota.report {
                for p in &r.providers {
                    if crate::quota::has_usage_windows(p) {
                        let suffix = if p.stale { " (stale)" } else { "" };
                        println!("quota {}{}", crate::quota_row::once_line(p, now), suffix);
                    }
                }
            }
        }
        crate::app::QuotaSelection::Explicit(ids) => {
            if ids.is_empty() {
                return;
            }
            match &app.quota.report {
                Some(r) => {
                    for p in &r.providers {
                        if ids.contains(&p.id) {
                            let suffix = if p.stale { " (stale)" } else { "" };
                            println!("quota {}{}", crate::quota_row::once_line(p, now), suffix);
                        }
                    }
                }
                None => {
                    let err = app.quota.last_error.as_deref().unwrap_or("unavailable");
                    println!("quota: unavailable ({})", err);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_interval_default_and_in_range_values_pass_through() {
        assert_eq!(resolve_quota_interval(300.0), Duration::from_secs(300));
        assert_eq!(resolve_quota_interval(90.0), Duration::from_secs(90));
    }

    #[test]
    fn quota_interval_below_floor_is_raised_to_60s() {
        assert_eq!(resolve_quota_interval(5.0), Duration::from_secs(60));
        assert_eq!(resolve_quota_interval(0.0), Duration::from_secs(60));
        assert_eq!(resolve_quota_interval(-30.0), Duration::from_secs(60));
    }

    #[test]
    fn quota_interval_above_ceiling_and_non_finite_are_bounded() {
        assert_eq!(resolve_quota_interval(99_999.0), Duration::from_secs(3600));
        assert_eq!(resolve_quota_interval(f64::NAN), Duration::from_secs(300));
        assert_eq!(
            resolve_quota_interval(f64::INFINITY),
            Duration::from_secs(300)
        );
    }
}
