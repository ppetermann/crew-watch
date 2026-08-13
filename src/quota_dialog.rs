//! Quota provider-selection dialog: a pure state machine over key codes.
//!
//! Opened with `p` from the main view. The item list is built at open time as
//! *(providers in the latest report, report order)* ∪ *(stored-selection ids no
//! longer reported, appended)* — never a hardcoded set, so a provider quota-axi
//! starts reporting (e.g. z.ai) appears with zero code change. While open, ticks
//! and quota updates continue underneath; the item list stays frozen so the
//! cursor never jumps mid-edit.

use crossterm::event::KeyCode;

use crate::quota::{has_usage_windows, QuotaReport};
use crate::quota_row::dialog_note;

/// One selectable row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogItem {
    pub id: String,
    /// Status note: `plan · status` for live providers, a short phrase for the
    /// rest, or `not reported` for ids in the stored selection absent from the
    /// report.
    pub note: String,
    pub selected: bool,
    /// True when this id is present in the current report (selectable live or
    /// selectable-but-failing). False for stored-but-vanished ids.
    pub reported: bool,
}

/// The dialog state. `cursor` is the highlighted row index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaDialog {
    pub items: Vec<DialogItem>,
    pub cursor: usize,
}

/// Result of a keypress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Key consumed; stay open.
    Pending,
    /// Persist these ids (the checked ones, in item order) and close.
    Save(Vec<String>),
    /// Discard and close.
    Cancel,
}

/// Build the dialog. `stored` is the persisted selection (`None` ⇒ auto mode).
/// In auto mode the providers reporting windows are pre-checked, so Enter with
/// no changes just makes the current view explicit. The `auto_ids` parameter is
/// that seed set; passing it explicitly keeps this function pure over its
/// arguments.
pub fn open(
    report: Option<&QuotaReport>,
    stored: Option<&[String]>,
    auto_ids: &[String],
) -> QuotaDialog {
    let mut items = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    if let Some(r) = report {
        for p in &r.providers {
            let selected = match stored {
                Some(s) => s.iter().any(|x| x == &p.id),
                None => auto_ids.iter().any(|x| x == &p.id),
            };
            items.push(DialogItem {
                id: p.id.clone(),
                note: dialog_note(p),
                selected,
                reported: true,
            });
            seen.push(p.id.clone());
        }
    }

    // Stored-selection ids no longer reported: appended, still selected, so a
    // vanished provider can be deselected and cleaned out of the config rather
    // than silently dropped from it.
    if let Some(s) = stored {
        for id in s {
            if !seen.iter().any(|x| x == id) {
                items.push(DialogItem {
                    id: id.clone(),
                    note: "not reported".to_string(),
                    selected: true,
                    reported: false,
                });
            }
        }
    }

    QuotaDialog { items, cursor: 0 }
}

impl QuotaDialog {
    /// Advance the key state machine. Cursor movement wraps so every item is
    /// reachable without fuss; on an empty list every key is a no-op pending.
    pub fn handle_key(&mut self, code: KeyCode) -> Outcome {
        if self.items.is_empty() {
            return match code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => Outcome::Cancel,
                _ => Outcome::Pending,
            };
        }
        let n = self.items.len();
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = (self.cursor + n - 1) % n;
                Outcome::Pending
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1) % n;
                Outcome::Pending
            }
            KeyCode::Char(' ') => {
                let i = self.cursor.min(n - 1);
                self.items[i].selected = !self.items[i].selected;
                Outcome::Pending
            }
            KeyCode::Enter => {
                let ids: Vec<String> = self
                    .items
                    .iter()
                    .filter(|it| it.selected)
                    .map(|it| it.id.clone())
                    .collect();
                Outcome::Save(ids)
            }
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Cancel,
            _ => Outcome::Pending,
        }
    }
}

/// The ids auto mode seeds the selection from (providers with ≥1 window), in
/// report order. Exposed so `main.rs` can pre-check the dialog and so the auto
/// row and the dialog share one definition of "auto".
pub fn auto_ids(report: Option<&QuotaReport>) -> Vec<String> {
    report
        .map(|r| {
            r.providers
                .iter()
                .filter(|p| has_usage_windows(p))
                .map(|p| p.id.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::{ProviderQuota, ProviderStatus, QuotaReport, QuotaWindow};
    fn live(id: &str, plan: Option<&str>) -> ProviderQuota {
        ProviderQuota {
            id: id.to_string(),
            label: id.to_string(),
            plan: plan.map(|p| p.to_string()),
            windows: vec![QuotaWindow {
                id: "five_hour".to_string(),
                label: "session".to_string(),
                percent_used: 10.0,
                resets_at: None,
            }],
            status: ProviderStatus::Fresh,
            stale: false,
            error: None,
        }
    }

    fn dead(id: &str, status: ProviderStatus) -> ProviderQuota {
        ProviderQuota {
            id: id.to_string(),
            label: id.to_string(),
            plan: None,
            windows: vec![],
            status,
            stale: false,
            error: Some("e".to_string()),
        }
    }

    fn report(providers: Vec<ProviderQuota>) -> QuotaReport {
        QuotaReport {
            schema_version: Some(3),
            generated_at: String::new(),
            providers,
        }
    }

    #[test]
    fn open_union_order_and_notes() {
        let r = report(vec![
            live("claude", Some("max")),
            dead("codex", ProviderStatus::Error),
            dead("copilot", ProviderStatus::AuthRequired),
        ]);
        let d = open(Some(&r), None, &["claude".to_string()]);
        assert_eq!(d.items.len(), 3);
        assert_eq!(d.items[0].id, "claude");
        assert_eq!(d.items[0].note, "max · fresh");
        assert!(d.items[0].reported);
        assert_eq!(d.items[1].id, "codex");
        assert_eq!(d.items[1].note, "unavailable");
        assert_eq!(d.items[2].note, "sign-in required");
    }

    #[test]
    fn open_auto_prechecks_live_only() {
        let r = report(vec![
            live("claude", None),
            dead("codex", ProviderStatus::Error),
        ]);
        let d = open(Some(&r), None, &["claude".to_string()]);
        assert!(d.items[0].selected, "claude pre-checked");
        assert!(!d.items[1].selected, "codex not checked in auto");
    }

    #[test]
    fn open_explicit_selection_prechecks_stored() {
        let r = report(vec![
            live("claude", None),
            dead("codex", ProviderStatus::Error),
        ]);
        let d = open(Some(&r), Some(&["codex".to_string()]), &[]);
        assert!(!d.items[0].selected);
        assert!(d.items[1].selected, "explicitly-selected codex checked");
    }

    #[test]
    fn open_appends_stored_but_absent_as_not_reported() {
        let r = report(vec![live("claude", None)]);
        // z.ai was selected before but quota-axi no longer reports it.
        let d = open(
            Some(&r),
            Some(&["claude".to_string(), "zai".to_string()]),
            &[],
        );
        assert_eq!(d.items.len(), 2);
        let zai = &d.items[1];
        assert_eq!(zai.id, "zai");
        assert!(!zai.reported);
        assert_eq!(zai.note, "not reported");
        assert!(zai.selected, "still selected so it can be deselected");
    }

    #[test]
    fn cursor_wraps_both_ways() {
        let r = report(vec![live("a", None), live("b", None), live("c", None)]);
        let mut d = open(Some(&r), None, &["a".into(), "b".into(), "c".into()]);
        assert_eq!(d.cursor, 0);
        d.handle_key(KeyCode::Up);
        assert_eq!(d.cursor, 2, "up from 0 wraps to last");
        d.handle_key(KeyCode::Down);
        assert_eq!(d.cursor, 0, "down from last wraps to 0");
    }

    #[test]
    fn toggle_marks_and_enter_saves_checked() {
        let r = report(vec![
            live("claude", None),
            dead("codex", ProviderStatus::Error),
        ]);
        let mut d = open(Some(&r), Some(&["claude".to_string()]), &[]);
        // move to codex and toggle it on
        d.handle_key(KeyCode::Down);
        assert_eq!(d.cursor, 1);
        d.handle_key(KeyCode::Char(' '));
        assert!(d.items[1].selected);
        match d.handle_key(KeyCode::Enter) {
            Outcome::Save(ids) => {
                assert_eq!(ids, vec!["claude".to_string(), "codex".to_string()]);
            }
            o => panic!("expected Save, got {o:?}"),
        }
    }

    #[test]
    fn enter_with_nothing_checked_saves_empty() {
        let r = report(vec![live("claude", None)]);
        // Explicit empty selection: claude unchecked, nothing appended.
        let mut d = open(Some(&r), Some(&[]), &[]);
        match d.handle_key(KeyCode::Enter) {
            Outcome::Save(ids) => assert!(ids.is_empty()),
            o => panic!("expected Save, got {o:?}"),
        }
    }

    #[test]
    fn esc_cancels() {
        let r = report(vec![live("claude", None)]);
        let mut d = open(Some(&r), None, &["claude".to_string()]);
        assert_eq!(d.handle_key(KeyCode::Esc), Outcome::Cancel);
        assert_eq!(d.handle_key(KeyCode::Char('q')), Outcome::Cancel);
    }

    #[test]
    fn unknown_key_is_pending() {
        let r = report(vec![live("claude", None)]);
        let mut d = open(Some(&r), None, &["claude".to_string()]);
        assert_eq!(d.handle_key(KeyCode::Char('x')), Outcome::Pending);
    }

    #[test]
    fn empty_list_keys_cancel_or_pending() {
        let mut d = open(None, None, &[]);
        assert_eq!(d.handle_key(KeyCode::Down), Outcome::Pending);
        assert_eq!(d.handle_key(KeyCode::Enter), Outcome::Cancel);
    }

    #[test]
    fn auto_ids_providers_with_windows_only() {
        let r = report(vec![
            live("claude", None),
            dead("codex", ProviderStatus::Error),
        ]);
        assert_eq!(auto_ids(Some(&r)), vec!["claude".to_string()]);
        assert!(auto_ids(None).is_empty());
    }

    #[test]
    fn auto_ids_seed_ignores_status() {
        // A provider serving cached windows (stale) or an unrecognised status is
        // still seeded — freshness never governs visibility.
        let mut stale_claude = live("claude", None);
        stale_claude.stale = true;
        stale_claude.status = ProviderStatus::Unknown("stale".to_string());
        let mut zai = live("zai", None);
        zai.status = ProviderStatus::Unknown("missing".to_string());
        let r = report(vec![stale_claude, zai]);
        assert_eq!(
            auto_ids(Some(&r)),
            vec!["claude".to_string(), "zai".to_string()]
        );
    }
}
