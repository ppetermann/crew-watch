//! About overlay: pure content assembly and geometry for the identity panel.
//!
//! Opened with `a` from the main view; `a`, `q` and `Esc` close it again (see
//! [`handle_key`] — the same dismiss reflex as the quota dialog, minus the
//! dialog's editing keys). While open it is a view, not a mode: ticks and
//! quota updates continue underneath and no underlying state changes. All of
//! that is enforced by `main.rs` key routing, which never reaches the main
//! view's keys while the overlay is up.

use crossterm::event::KeyCode;
use ratatui::layout::Rect;

/// Package version baked in at build time (matches `--version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Upstream repository link (also `Cargo.toml`'s `repository`).
pub const REPO_URL: &str = "https://github.com/ppetermann/crew-watch";

/// Preferred panel width, bounded by the terminal (4 columns of breathing
/// room, matching the quota dialog's margin).
pub fn panel_width(area: Rect) -> u16 {
    62u16.min(area.width.saturating_sub(4))
}

/// Result of a keypress while the overlay is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Close the overlay and return to the main view.
    Close,
    /// Key consumed; stay open.
    Pending,
}

/// The overlay's key semantics: the trigger key plus the dialog's dismiss set
/// (`q`, `Esc`) close it; every other key is consumed as a no-op so nothing
/// leaks through to the main view while the overlay is up. `Ctrl-C` still
/// quits — `main.rs` checks it before routing here, as for the dialog.
pub fn handle_key(code: KeyCode) -> Outcome {
    match code {
        KeyCode::Char('a') | KeyCode::Char('q') | KeyCode::Esc => Outcome::Close,
        _ => Outcome::Pending,
    }
}

/// Body lines for the overlay, with the prose hard-wrapped to `inner_width`
/// so the panel degrades readably on narrow terminals: the description and
/// license notice re-wrap, while the identity lines (name+version, repository
/// link) never wrap and simply truncate if even narrower than themselves.
/// The dim footer hint line is appended by the renderer, mirroring the quota
/// dialog, and accounted for by [`centered_rect`].
pub fn about_lines(inner_width: usize) -> Vec<String> {
    let mut lines = vec![format!("crew-watch {VERSION}")];
    lines.push(String::new());
    lines.extend(wrap(DESCRIPTION, inner_width));
    lines.push(String::new());
    lines.push(REPO_URL.to_string());
    lines.push(String::new());
    for chunk in LICENSE_NOTICE {
        lines.extend(wrap(chunk, inner_width));
    }
    lines
}

/// Centered geometry for the overlay over `area`: `min(PANEL_WIDTH,
/// area.width-4)` wide, tall enough for `body_lines` plus the blank line,
/// footer hint and borders. Height clamps to the terminal so a short terminal
/// crops the panel bottom-up instead of panicking; `None` when there is no
/// room to draw anything.
pub fn centered_rect(area: Rect, body_lines: usize) -> Option<Rect> {
    let h = (body_lines as u16 + 4).min(area.height);
    let w = panel_width(area);
    if h == 0 || w == 0 {
        return None;
    }
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Some(Rect::new(x, y, w, h))
}

/// Hard word-wrap `text` to `width` columns. A word longer than `width`
/// occupies its own overflowing line rather than being split — the overlay's
/// identity lines are the only such words and are never passed through here.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

const DESCRIPTION: &str = "A terminal monitor for firstmate fleets: an \
htop-style system overview plus one row per running AI agent session, with \
CPU and memory aggregated over the agent's whole process subtree, the model, \
elapsed time, the task it is working on, and its current activity.";

/// Compact MIT notice; the full text deliberately stays in the LICENSE file.
const LICENSE_NOTICE: &[&str] = &[
    "MIT License — Copyright (c) 2026 Peter Petermann",
    "Full license text: the LICENSE file in the repository.",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_q_and_esc_close_everything_else_pending() {
        assert_eq!(handle_key(KeyCode::Char('a')), Outcome::Close);
        assert_eq!(handle_key(KeyCode::Char('q')), Outcome::Close);
        assert_eq!(handle_key(KeyCode::Esc), Outcome::Close);
        // Consumed no-ops: the overlay must never leak keys to the main view.
        assert_eq!(handle_key(KeyCode::Char('p')), Outcome::Pending);
        assert_eq!(handle_key(KeyCode::Enter), Outcome::Pending);
        assert_eq!(handle_key(KeyCode::Up), Outcome::Pending);
        assert_eq!(handle_key(KeyCode::Char(' ')), Outcome::Pending);
    }

    #[test]
    fn body_carries_version_link_and_license_notice() {
        let lines = about_lines(58);
        assert_eq!(
            lines.first().map(String::as_str),
            Some(format!("crew-watch {VERSION}").as_str())
        );
        assert!(lines.contains(&REPO_URL.to_string()));
        let joined = lines.join("\n");
        assert!(joined.contains("MIT License"));
        assert!(joined.contains("Copyright (c) 2026 Peter Petermann"));
    }

    #[test]
    fn wrapped_lines_never_exceed_the_requested_width() {
        for width in [45, 58, 60, 80] {
            for line in about_lines(width) {
                assert!(
                    line.chars().count() <= width,
                    "width {width}: {:?} is {} cols",
                    line,
                    line.chars().count()
                );
            }
        }
    }

    #[test]
    fn wrapping_preserves_every_word_in_order() {
        for width in [1, 8, 20, 58] {
            let lines = about_lines(width);
            // The description is the run of lines between the first blank
            // line and the blank line before the repo link.
            let start = 2; // title + blank
            let end = lines
                .iter()
                .position(|l| l == REPO_URL)
                .expect("repo link present")
                - 1; // the blank line in front of it
            let rejoined = lines[start..end].join(" ");
            assert_eq!(
                rejoined, DESCRIPTION,
                "wrap+rejoin must reproduce the source text at width {width}"
            );
        }
    }

    #[test]
    fn narrow_width_degrades_without_panicking() {
        // Far below any real terminal: no panic, identity lines still intact
        // (the renderer truncates over-width lines rather than wrapping them).
        let lines = about_lines(6);
        assert_eq!(
            lines.first().map(String::as_str),
            Some(format!("crew-watch {VERSION}").as_str())
        );
        assert!(lines.contains(&REPO_URL.to_string()));
    }

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn centered_rect_is_centered_at_preferred_size() {
        let body = about_lines(60);
        let r = centered_rect(rect(0, 0, 100, 30), body.len()).unwrap();
        assert_eq!(r.width, 62);
        assert_eq!(r.height, body.len() as u16 + 4);
        assert_eq!(r.x, (100 - 62) / 2);
        assert_eq!(r.y, (30 - r.height) / 2);
    }

    #[test]
    fn centered_rect_shrinks_on_narrow_terminal() {
        let r = centered_rect(rect(0, 0, 40, 30), 10).unwrap();
        assert_eq!(r.width, 36);
        assert_eq!(r.x, 2);
    }

    #[test]
    fn centered_rect_clamps_height_and_docks_to_top() {
        let r = centered_rect(rect(0, 0, 100, 10), 20).unwrap();
        assert_eq!(r.height, 10, "clamped to terminal height");
        assert_eq!(r.y, 0, "no room to center, docks to top");
    }

    #[test]
    fn centered_rect_none_when_no_room() {
        assert!(centered_rect(rect(0, 0, 0, 30), 10).is_none());
        assert!(centered_rect(rect(0, 0, 100, 0), 10).is_none());
        assert!(
            centered_rect(rect(0, 0, 4, 30), 10).is_none(),
            "4-wide leaves no inner room"
        );
        assert!(
            centered_rect(rect(0, 0, 80, 24), 0).is_some(),
            "empty body still draws the shell"
        );
    }
}
