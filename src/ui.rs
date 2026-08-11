//! TUI rendering: a htop-style system overview on top and an agent list below.
//!
//! Rendering itself is intentionally untested in v1; all the pure logic it
//! depends on (parsing, detection, aggregation, task resolution) is unit-tested
//! in the other modules.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::App;
use crate::detect::{AgentKind, AGENT_KINDS};
use crate::format_util::{format_duration, format_kib, format_percent, format_uptime};
use crate::procfs::{CpuLine, Snapshot};
use crate::taskinfo::fit_task_line;

const MIN_CELL_WIDTH: usize = 18;
const MAX_COLS: usize = 8;
const MEM_BAR_WIDTH: usize = 20;
/// Fixed widths of every agent-table column before the flexible TASK column.
const AGENT_FIXED_COL_WIDTHS: [u16; 6] = [10, 14, 8, 10, 9, 12];
const AGENT_COL_SPACING: u16 = 1;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let top_h = top_height(app.snapshot(), area.width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_h),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);
    draw_top(f, chunks[0], app);
    draw_agents(f, chunks[1], app);
    draw_help(f, chunks[2], app);
}

/// Height the top panel needs given terminal width (core grid is columnar).
fn top_height(curr: Option<&Snapshot>, width: u16) -> u16 {
    let Some(curr) = curr else {
        return 4;
    };
    let cores = num_cores(curr);
    let cols = cols_for_width(width as usize);
    let rows = cores.div_ceil(cols).max(1);
    // cpu grid rows + mem line + info line + 2 border rows.
    (rows + 2 + 2) as u16
}

fn num_cores(snap: &Snapshot) -> usize {
    snap.cpus
        .iter()
        .filter(|c| !c.is_aggregate())
        .count()
        .max(1)
}

fn cols_for_width(inner_width_px: usize) -> usize {
    let usable = inner_width_px.saturating_sub(2);
    let cols = usable / MIN_CELL_WIDTH;
    cols.clamp(1, MAX_COLS)
}

fn draw_top(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("System")
        .title_bottom(Line::from("crew-watch").right_aligned());
    let lines = match app.snapshot() {
        Some(snap) => build_top_lines(snap, app.prev.as_ref(), area.width as usize),
        None => vec![Line::from("collecting /proc ...")],
    };
    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}

fn build_top_lines(curr: &Snapshot, prev: Option<&Snapshot>, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Per-core usage grid.
    let cores: Vec<&CpuLine> = curr.cpus.iter().filter(|c| !c.is_aggregate()).collect();
    let pcts = core_pcts(curr, prev);
    if !cores.is_empty() {
        let n = cores.len();
        let cols = cols_for_width(width);
        let cell_w = (width.saturating_sub(2)) / cols;
        let bar_w = cell_w.saturating_sub(9).max(4);
        let rows = n.div_ceil(cols);
        for r in 0..rows {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for c in 0..cols {
                let idx = c * rows + r;
                if idx < n {
                    let corenum = idx + 1;
                    let pct = pcts.get(idx).copied().unwrap_or(0.0);
                    let cell = format_cell(corenum, pct, bar_w, cell_w);
                    spans.push(Span::styled(cell, Style::default().fg(pct_color(pct))));
                } else {
                    spans.push(Span::raw(" ".repeat(cell_w)));
                }
            }
            lines.push(Line::from(spans));
        }
    }

    // Memory + swap.
    let mem = &curr.mem;
    let mem_used = mem.mem_total_kib.saturating_sub(mem.mem_avail_kib);
    let mem_pct = ratio(mem_used, mem.mem_total_kib);
    let swap_used = mem.swap_total_kib.saturating_sub(mem.swap_free_kib);
    let swap_pct = ratio(swap_used, mem.swap_total_kib);
    let mem_label = format!(
        "{} / {}",
        format_kib(mem_used),
        format_kib(mem.mem_total_kib)
    );
    let swap_label = format!(
        "{} / {}",
        format_kib(swap_used),
        format_kib(mem.swap_total_kib)
    );
    lines.push(Line::from(vec![
        Span::raw("Mem "),
        Span::styled(
            format!("{} ", make_bar(mem_pct, MEM_BAR_WIDTH)),
            Style::default().fg(pct_color(mem_pct)),
        ),
        Span::styled(
            format!("{:>5.0}% ", mem_pct),
            Style::default().fg(pct_color(mem_pct)),
        ),
        Span::raw(mem_label),
        Span::raw("   Swp "),
        Span::styled(
            format!("{} ", make_bar(swap_pct, MEM_BAR_WIDTH)),
            Style::default().fg(pct_color(swap_pct)),
        ),
        Span::styled(
            format!("{:>5.0}% ", swap_pct),
            Style::default().fg(pct_color(swap_pct)),
        ),
        Span::raw(swap_label),
    ]));

    // Tasks / load / uptime.
    let up_secs = curr.uptime.secs.max(0.0) as u64;
    lines.push(Line::from(format!(
        "Tasks: {} total, {} running    Load avg: {:.2} {:.2} {:.2}    Uptime: {}",
        curr.load.total,
        curr.load.running,
        curr.load.one,
        curr.load.five,
        curr.load.fifteen,
        format_uptime(up_secs)
    )));

    lines
}

/// Per-core busy percentages (0..=100), matched core-by-core against the
/// previous snapshot. Cores without a previous sample read 0%.
fn core_pcts(curr: &Snapshot, prev: Option<&Snapshot>) -> Vec<f64> {
    let cores: Vec<&CpuLine> = curr.cpus.iter().filter(|c| !c.is_aggregate()).collect();
    let prev_map: std::collections::HashMap<&str, &CpuLine> = prev
        .map(|p| {
            p.cpus
                .iter()
                .filter(|c| !c.is_aggregate())
                .map(|c| (c.name.as_str(), c))
                .collect()
        })
        .unwrap_or_default();
    cores
        .iter()
        .map(|c| {
            let Some(p) = prev_map.get(c.name.as_str()) else {
                return 0.0;
            };
            let total_d = c.total().saturating_sub(p.total());
            if total_d == 0 {
                return 0.0;
            }
            let idle_d = c.idle().saturating_sub(p.idle());
            let busy_d = total_d.saturating_sub(idle_d);
            (busy_d as f64 / total_d as f64) * 100.0
        })
        .collect()
}

fn format_cell(corenum: usize, pct: f64, bar_w: usize, cell_w: usize) -> String {
    let mut s = format!("{:>2} {:>3.0}% {}", corenum, pct, make_bar(pct, bar_w));
    if s.chars().count() < cell_w {
        let pad = cell_w - s.chars().count();
        s.push_str(&" ".repeat(pad));
    } else if s.chars().count() > cell_w {
        s = s.chars().take(cell_w).collect();
    }
    s
}

fn make_bar(pct: f64, width: usize) -> String {
    let width = width.max(1);
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push('\u{2588}'); // full block
    }
    for _ in filled..width {
        s.push('\u{2591}'); // light shade
    }
    s
}

fn ratio(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) * 100.0
    }
}

fn pct_color(pct: f64) -> Color {
    if pct >= 90.0 {
        Color::Red
    } else if pct >= 60.0 {
        Color::Yellow
    } else if pct >= 30.0 {
        Color::Green
    } else {
        Color::Cyan
    }
}

fn draw_agents(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(format!(
        "Agents ({} sessions) [sorted by CPU% desc]",
        app.sessions.len()
    ));

    if app.sessions.is_empty() {
        let p = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No agent runtimes detected.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  crew-watch scans /proc for: claude, opencode, codex, grok, kimi, muse, pi.",
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(block);
        f.render_widget(p, area);
        return;
    }

    let header = Row::new(vec![
        Cell::from("RUNTIME"),
        Cell::from("MODEL"),
        Cell::from("PID"),
        Cell::from("ELAPSED"),
        Cell::from("CPU%"),
        Cell::from("MEM"),
        Cell::from("TASK"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    // Width the TASK column actually gets: total minus borders, the fixed
    // columns, and the spacing between all seven columns.
    let fixed: u16 = AGENT_FIXED_COL_WIDTHS.iter().sum();
    let spacing = AGENT_COL_SPACING * AGENT_FIXED_COL_WIDTHS.len() as u16;
    let task_width = area.width.saturating_sub(2 + fixed + spacing).max(1) as usize;

    let rows = app.sessions.iter().map(|s| {
        Row::new(vec![
            Cell::from(s.kind.display.to_string()).style(Style::default().fg(kind_color(s.kind))),
            Cell::from(if s.model.is_empty() {
                "-".to_string()
            } else {
                s.model.clone()
            }),
            Cell::from(s.pid.to_string()),
            Cell::from(format_duration(s.elapsed_secs)),
            Cell::from(format_percent(s.cpu_percent))
                .style(Style::default().fg(pct_color(s.cpu_percent))),
            Cell::from(format_kib(s.rss_kib)),
            Cell::from(fit_task_line(&s.task, task_width)),
        ])
    });

    let mut constraints: Vec<Constraint> = AGENT_FIXED_COL_WIDTHS
        .iter()
        .map(|w| Constraint::Length(*w))
        .collect();
    constraints.push(Constraint::Min(1));
    let table = Table::new(rows, constraints)
        .column_spacing(AGENT_COL_SPACING)
        .header(header)
        .block(block);
    f.render_widget(table, area);
}

fn kind_color(kind: &AgentKind) -> Color {
    match kind.id {
        "claude" => Color::Magenta,
        "opencode" => Color::Cyan,
        "codex" => Color::Blue,
        "grok" => Color::LightGreen,
        "kimi" => Color::LightBlue,
        "muse" => Color::LightYellow,
        "pi" => Color::LightRed,
        _ => Color::Green,
    }
}

fn draw_help(f: &mut Frame, area: Rect, app: &App) {
    let interval = app.interval.as_secs_f64();
    let agent_ids: Vec<&str> = AGENT_KINDS.iter().map(|k| k.id).collect();
    let line = Line::from(format!(
        " q/Esc/Ctrl-C quit  |  refresh {:.1}s  |  detecting: {}",
        interval,
        agent_ids.join(", "),
    ));
    f.render_widget(Paragraph::new(line), area);
}
