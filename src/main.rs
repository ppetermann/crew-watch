//! crew-watch entry point: argument parsing, terminal setup/teardown, and the
//! render/tick event loop.

mod app;
mod cli;
mod detect;
mod format_util;
mod meta;
mod model;
mod procfs;
mod project;
mod taskinfo;
mod titles;
mod ui;

use std::io;
use std::path::PathBuf;
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
use crate::meta::load_fm_home;
use crate::titles::load_task_titles;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let interval_secs = if cli.interval.is_finite() {
        cli.interval.clamp(0.1, 3600.0)
    } else {
        2.0
    };
    let interval = Duration::from_secs_f64(interval_secs);
    let fm_home = resolve_fm_home(cli.fm_home);
    let records = load_fm_home(&fm_home);
    let titles = load_task_titles(&fm_home);
    let mut app = App::new(interval, records, titles);

    if cli.once {
        // Two samples ~1s apart so CPU% deltas are real (not all zero).
        app.tick();
        std::thread::sleep(Duration::from_millis(1000));
        app.tick();
        print_once(&app);
        return Ok(());
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

/// Render + input/tick loop. Polls with a timeout of the remaining time until
/// the next refresh, so input stays responsive while /proc is read at most once
/// per interval.
fn run<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        let timeout = app.interval.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(k) = event::read()? {
                if is_quit(&k) {
                    return Ok(());
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
        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => true,
        _ => false,
    }
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

/// Non-interactive text dump of one snapshot (the `--once` path).
fn print_once(app: &App) {
    use crate::format_util::{format_duration, format_kib, format_percent, format_uptime};
    use crate::procfs::CpuLine;

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
        return;
    }
    println!(
        "{:<10} {:<14} {:>7} {:>10} {:>9} {:>12}  TASK",
        "RUNTIME", "MODEL", "PID", "ELAPSED", "CPU%", "MEM"
    );
    for s in &app.sessions {
        let model = if s.model.is_empty() {
            "-".to_string()
        } else {
            s.model.clone()
        };
        println!(
            "{:<10} {:<14} {:>7} {:>10} {:>9} {:>12}  {}",
            s.kind.display,
            model,
            s.pid,
            format_duration(s.elapsed_secs),
            format_percent(s.cpu_percent),
            format_kib(s.rss_kib),
            s.task
        );
    }
}
