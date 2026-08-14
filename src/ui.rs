//! TUI rendering: a htop-style system overview on top and an agent list below.
//!
//! Rendering itself is intentionally untested in v1; all the pure logic it
//! depends on (parsing, detection, aggregation, task resolution) is unit-tested
//! in the other modules.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{App, QuotaSelection};
use crate::detect::{AgentKind, AGENT_KINDS};
use crate::format_util::{
    format_age_compact, format_duration, format_kib, format_percent, format_uptime, make_bar,
};
use crate::procfs::{CpuLine, Snapshot};
use crate::quota::{has_usage_windows, ProviderQuota};
use crate::quota_row::build_quota_rows;
use crate::taskinfo::fit_task_line;
const MIN_CELL_WIDTH: usize = 18;
const MAX_COLS: usize = 8;
const MEM_BAR_WIDTH: usize = 20;
/// Fixed widths of every agent-table column before the flexible TASK column.
const AGENT_FIXED_COL_WIDTHS: [u16; 6] = [10, 14, 8, 10, 9, 12];
const AGENT_COL_SPACING: u16 = 1;

/// Right-aligned cell for the agent table's numeric columns (PID, ELAPSED,
/// CPU%, MEM): their magnitudes stack on the right edge so rows compare at a
/// glance. PID joins them — like htop and `ps`, and matching `--once` — even
/// though it is an identifier, not a magnitude; digits read as one numeric
/// block. Text columns (RUNTIME, MODEL, TASK) stay left-aligned, and
/// `--once` formats the same columns right-aligned, so the two surfaces stay
/// consistent. Over-width content truncates (alignment ignored), exactly as
/// the left-aligned cells always have.
fn right_cell(text: String) -> Cell<'static> {
    Cell::from(Text::from(text).alignment(Alignment::Right))
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let top_h = top_height(app.snapshot(), area.width);
    let quota_h = app.quota_lines_count() as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_h),
            Constraint::Min(4),
            Constraint::Length(quota_h),
            Constraint::Length(1),
        ])
        .split(area);
    draw_top(f, chunks[0], app);
    draw_agents(f, chunks[1], app);
    draw_quota(f, chunks[2], app);
    draw_help(f, chunks[3], app);
    // The dialog overlays the agents area, rendered last so it sits on top.
    if let Some(dialog) = app.dialog.as_ref() {
        if let Some(rect) = centered_dialog(area, dialog.items.len()) {
            draw_dialog(f, rect, app);
        }
    }
}

/// Current UTC epoch seconds (for quota reset countdowns at render time).
fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
        right_cell("PID".to_string()),
        right_cell("ELAPSED".to_string()),
        right_cell("CPU%".to_string()),
        right_cell("MEM".to_string()),
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
            right_cell(s.pid.to_string()),
            right_cell(format_duration(s.elapsed_secs)),
            right_cell(format_percent(s.cpu_percent))
                .style(Style::default().fg(pct_color(s.cpu_percent))),
            right_cell(format_kib(s.rss_kib)),
            Cell::from(fit_task_line(
                s.task_project.as_deref(),
                &s.task,
                task_width,
            )),
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
    // A transient notice (e.g. config save failed) takes over the help line
    // until the next keypress clears it; otherwise show the normal bindings,
    // with `p providers` present only when the quota row is enabled.
    if let Some(notice) = &app.notice {
        let line = Line::from(Span::styled(
            format!(" {notice}"),
            Style::default().fg(Color::Yellow),
        ));
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    let providers_binding = if app.quota_enabled {
        "  |  p providers"
    } else {
        ""
    };
    let line = Line::from(format!(
        " q/Esc/Ctrl-C quit{providers_binding}  |  refresh {:.1}s  |  detecting: {}",
        interval,
        agent_ids.join(", "),
    ));
    f.render_widget(Paragraph::new(line), area);
}

// ---------------------------------------------------------------------------
// Quota row
// ---------------------------------------------------------------------------

fn draw_quota(f: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    let now = current_epoch_secs();
    let row_age = app.quota_fetch_age();
    let age_suffix = row_age.map(|d| format!(" ({} old)", format_age_compact(d.as_secs())));

    // Collect the providers to display, in display order, then build the whole
    // block through `build_quota_rows` so live rows share aligned columns.
    let mut providers: Vec<&ProviderQuota> = Vec::new();
    let mut have_report = true;
    match app.effective_selection() {
        QuotaSelection::Auto => {
            if let Some(r) = &app.quota.report {
                for p in r.providers.iter().filter(|p| has_usage_windows(p)) {
                    providers.push(p);
                }
            }
        }
        QuotaSelection::Explicit(ids) => {
            if !ids.is_empty() {
                match &app.quota.report {
                    Some(r) => {
                        for p in r.providers.iter().filter(|p| ids.contains(&p.id)) {
                            providers.push(p);
                        }
                    }
                    None => have_report = false,
                }
            }
        }
    }

    let mut lines: Vec<Line<'static>> = Vec::new();

    // D9: explicit selection with no report ⇒ one dim failure line (no rows).
    if !have_report {
        let err = app.quota.last_error.as_deref().unwrap_or("unavailable");
        lines.push(Line::from(Span::styled(
            format!(" quota: unavailable ({err})"),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        let block = build_quota_rows(&providers, now, area.width as usize);
        for (p, segs) in providers.iter().zip(block.iter()) {
            let dim = row_age.is_some() || p.stale;
            let live = has_usage_windows(p);
            let spans: Vec<Span<'static>> = segs
                .iter()
                .enumerate()
                .map(|(i, seg)| {
                    if dim || !live {
                        Span::styled(seg.text.clone(), Style::default().fg(Color::DarkGray))
                    } else if i == 0 {
                        Span::styled(seg.text.clone(), Style::default().fg(runtime_color(&p.id)))
                    } else if let Some(pct) = seg.pct {
                        Span::styled(seg.text.clone(), Style::default().fg(pct_color(pct)))
                    } else {
                        Span::raw(seg.text.clone())
                    }
                })
                .collect();
            let mut line = Line::from(spans);
            if p.stale {
                line.spans
                    .push(Span::styled(" stale", Style::default().fg(Color::DarkGray)));
            }
            lines.push(line);
        }
    }

    // Whole-row fetch staleness: append the age suffix to the first line.
    if let Some(suffix) = &age_suffix {
        if let Some(line) = lines.first_mut() {
            line.spans.push(Span::styled(
                suffix.clone(),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    if !lines.is_empty() {
        f.render_widget(Paragraph::new(lines), area);
    }
}

/// Color for a provider/runtime id, reusing the agents-table palette. Unknown
/// ids default to `Color::White` so a future provider (e.g. z.ai) is visible.
fn runtime_color(id: &str) -> Color {
    match id {
        "claude" => Color::Magenta,
        "opencode" => Color::Cyan,
        "codex" => Color::Blue,
        "grok" => Color::LightGreen,
        "kimi" => Color::LightBlue,
        "muse" => Color::LightYellow,
        "pi" => Color::LightRed,
        _ => Color::White,
    }
}

// ---------------------------------------------------------------------------
// Quota provider-selection dialog overlay
// ---------------------------------------------------------------------------

/// Centered rect for the dialog: width `min(50, area.width-4)`, height
/// `items+4` (border + header gap + footer + items). Returns `None` when there
/// is no room.
fn centered_dialog(area: Rect, item_count: usize) -> Option<Rect> {
    let h = (item_count as u16 + 4).min(area.height);
    let w = (50u16).min(area.width.saturating_sub(4));
    if h == 0 || w == 0 {
        return None;
    }
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Some(Rect::new(x, y, w, h))
}

fn draw_dialog(f: &mut Frame, area: Rect, app: &App) {
    let dialog = app.dialog.as_ref().expect("dialog present");
    // Clear the background under the overlay so the agents table doesn't show
    // through.
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Quota providers");

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, item) in dialog.items.iter().enumerate() {
        let mark = if item.selected { "[x]" } else { "[ ]" };
        let cursor = if i == dialog.cursor { "▶" } else { " " };
        let checkbox = format!(" {cursor}{mark} {:<10} {}", item.id, item.note);
        let style = if i == dialog.cursor {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if !item.reported {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(checkbox, style)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " space toggle · enter save · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines).block(block), area);
}
