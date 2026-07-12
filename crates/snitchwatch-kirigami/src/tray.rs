//! Tray-state → tooltip/menu-label derivation (Task 18).
//!
//! Ported unchanged from `snitchwatch-tauri::tray`'s `derive_tooltip` /
//! `derive_menu_label` / `MenuLabel` — these are pure functions over the
//! bridge's `TrayState` enum with zero Tauri dependency (the Tauri-specific
//! part was only `Tray::install`'s `tauri::tray::TrayIcon` wiring, which the
//! Kirigami shell replaces with `Qt.labs.platform.SystemTrayIcon` in QML —
//! see `TrayController` for the thin cxx-qt wrapper that feeds these into a
//! `#[qproperty]`).
//!
//! Their 7 unit tests below transfer unchanged.

use snitchwatch_bridge::tray_state::TrayState;
use snitchwatch_bridge::ws_messages::ClientMessage;

pub fn derive_tooltip(state: &TrayState) -> String {
    match state {
        TrayState::Idle => "Snitchwatch — filtering".into(),
        TrayState::Pending(n) => format!("{n} pending decisions"),
        TrayState::RecentBlock { what, .. } => format!("Blocked: {what}"),
        TrayState::FilterOff => "Snitchwatch — filtering disabled".into(),
        TrayState::DaemonDown => "opensnitchd not reachable".into(),
    }
}

#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MenuLabel {
    Default,
    PauseFiltering,
    ResumeFiltering,
    Reconnect,
}

pub fn derive_menu_label(state: &TrayState) -> MenuLabel {
    match state {
        TrayState::FilterOff => MenuLabel::ResumeFiltering,
        TrayState::DaemonDown => MenuLabel::Reconnect,
        TrayState::Idle | TrayState::Pending(_) | TrayState::RecentBlock { .. } => {
            MenuLabel::PauseFiltering
        }
    }
}

/// The menu-label token surfaced to QML (see `TrayController::menu_label`).
/// Kept separate from `MenuLabel`'s `Debug` output so the QML-facing string
/// is a stable contract independent of how the Rust enum is printed.
pub fn menu_label_token(label: &MenuLabel) -> &'static str {
    match label {
        MenuLabel::Default => "default",
        MenuLabel::PauseFiltering => "pause_filtering",
        MenuLabel::ResumeFiltering => "resume_filtering",
        MenuLabel::Reconnect => "reconnect",
    }
}

/// Build the JSON-encoded `ClientMessage::SetFilteringPaused` the tray menu's
/// "Pause/Resume filtering" item sends via `BridgeFeed::sendClientJson` (the
/// same in-process path `PendingDecision::submit` and friends use — see
/// `docs/superpowers/plans/2026-07-12-tray-filter-off.md`). Pure and
/// infallible: unlike `PendingDecision`'s verdict-building, there's no
/// free-text QML input to validate here, just a bool.
pub fn build_set_filtering_paused_json(paused: bool) -> String {
    serde_json::to_string(&ClientMessage::SetFilteringPaused { paused })
        .expect("ClientMessage::SetFilteringPaused always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn tooltip_idle() {
        assert_eq!(derive_tooltip(&TrayState::Idle), "Snitchwatch — filtering");
    }

    #[test]
    fn tooltip_pending_uses_count() {
        assert_eq!(
            derive_tooltip(&TrayState::Pending(3)),
            "3 pending decisions"
        );
    }

    #[test]
    fn tooltip_recent_block_includes_what() {
        let s = TrayState::RecentBlock {
            what: "spotify → tracker.x".into(),
            ttl: Duration::from_secs(3),
        };
        assert_eq!(derive_tooltip(&s), "Blocked: spotify → tracker.x");
    }

    #[test]
    fn tooltip_filter_off() {
        assert_eq!(
            derive_tooltip(&TrayState::FilterOff),
            "Snitchwatch — filtering disabled"
        );
    }

    #[test]
    fn tooltip_daemon_down() {
        assert_eq!(
            derive_tooltip(&TrayState::DaemonDown),
            "opensnitchd not reachable"
        );
    }

    #[test]
    fn menu_label_filter_off_offers_resume() {
        assert_eq!(
            derive_menu_label(&TrayState::FilterOff),
            MenuLabel::ResumeFiltering
        );
    }

    #[test]
    fn menu_label_daemon_down_offers_reconnect() {
        assert_eq!(
            derive_menu_label(&TrayState::DaemonDown),
            MenuLabel::Reconnect
        );
    }

    #[test]
    fn set_filtering_paused_json_round_trips() {
        let json = build_set_filtering_paused_json(true);
        assert_eq!(json, r#"{"action":"setFilteringPaused","paused":true}"#);
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ClientMessage::SetFilteringPaused { paused: true });
    }
}
