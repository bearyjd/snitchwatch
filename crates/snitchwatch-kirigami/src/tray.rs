//! Tray-state → tooltip/menu-label derivation (Task 18).
//!
//! Ported from `snitchwatch-tauri::tray`'s `derive_tooltip` /
//! `derive_menu_label` / `MenuLabel` — pure functions over the bridge's
//! `TrayState` with zero Tauri dependency (the Tauri-specific part was only
//! `Tray::install`'s `tauri::tray::TrayIcon` wiring, which the Kirigami shell
//! replaces with `Qt.labs.platform.SystemTrayIcon` in QML — see
//! `TrayController` for the thin cxx-qt wrapper that feeds these into a
//! `#[qproperty]`).
//!
//! Issue #47 adds the bridge's `FilterPauseState` as a second input: a pause
//! is timed, and the tray shows when it ends.

use snitchwatch_bridge::filter_pause::PauseState;
use snitchwatch_bridge::tray_state::TrayState;
use snitchwatch_bridge::ws_messages::ClientMessage;

/// Paused per the bridge's `FilterPauseState`, or per `FilterOff` from a
/// bridge that predates it. While paused the bridge shows `FilterOff` except
/// for a transient `RecentBlock` or a `DaemonDown`, so the pause state, not
/// the tray state, decides.
fn is_paused(state: &TrayState, pause: &PauseState) -> bool {
    pause.paused || *state == TrayState::FilterOff
}

/// `paused_until` is the pause's local end time ("HH:MM"), or empty when
/// unknown (a bridge that predates `FilterPauseState`).
pub fn derive_tooltip(state: &TrayState, pause: &PauseState, paused_until: &str) -> String {
    match state {
        TrayState::DaemonDown => "opensnitchd not reachable".into(),
        TrayState::RecentBlock { what, .. } => format!("Blocked: {what}"),
        _ if is_paused(state, pause) && !paused_until.is_empty() => {
            format!("Snitchwatch — filtering paused until {paused_until}")
        }
        _ if is_paused(state, pause) => "Snitchwatch — filtering disabled".into(),
        TrayState::Pending(n) => format!("{n} pending decisions"),
        TrayState::Idle | TrayState::FilterOff => "Snitchwatch — filtering".into(),
    }
}

/// [`derive_tooltip`], plus issue #78: while paused, prompts holding the
/// daemon's single slot mean the pause can't reach other new connections,
/// and the tooltip says so. Fixed text only (the tooltip renders rich text).
pub fn derive_tooltip_with_slot(
    state: &TrayState,
    pause: &PauseState,
    paused_until: &str,
    slot_holders: u32,
) -> String {
    let slot_held = slot_holders > 0;
    let shows_pause = !matches!(state, TrayState::DaemonDown | TrayState::RecentBlock { .. });
    if slot_held && shows_pause && is_paused(state, pause) {
        let until = if paused_until.is_empty() {
            String::new()
        } else {
            format!(" until {paused_until}")
        };
        return format!(
            "Snitchwatch — filtering paused{until}. {}",
            crate::prompt_slot_text::paused_while_waiting(slot_holders)
        );
    }
    derive_tooltip(state, pause, paused_until)
}

/// The prompt-slot holders the tray may speak of: the received count only
/// while it belongs to the live session.
pub fn live_slot_holders(holders: u32, session_is_current: bool) -> u32 {
    if session_is_current {
        holders
    } else {
        0
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

/// While paused, always offer Resume — even over a `DaemonDown` — so the
/// user can end a pause at any time.
pub fn derive_menu_label(state: &TrayState, pause: &PauseState) -> MenuLabel {
    if is_paused(state, pause) {
        return MenuLabel::ResumeFiltering;
    }
    match state {
        TrayState::DaemonDown => MenuLabel::Reconnect,
        TrayState::Idle
        | TrayState::Pending(_)
        | TrayState::RecentBlock { .. }
        | TrayState::FilterOff => MenuLabel::PauseFiltering,
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
/// "Pause for …" items and "Resume filtering" item send via
/// `BridgeFeed::sendClientJson` (the same in-process path
/// `BridgeFeed::submitVerdict` and friends use — see
/// `docs/superpowers/plans/2026-07-12-tray-filter-off.md`). Pure and
/// infallible; the bridge validates the duration (issue #47).
pub fn build_set_filtering_paused_json(paused: bool, duration_secs: Option<u64>) -> String {
    serde_json::to_string(&ClientMessage::SetFilteringPaused {
        paused,
        duration_secs,
        sender_generation: None,
        sender_uid: None,
    })
    .expect("ClientMessage::SetFilteringPaused always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const NOT_PAUSED: PauseState = PauseState::NOT_PAUSED;
    const PAUSED: PauseState = PauseState {
        paused: true,
        expires_at_unix_ms: Some(1_800_000_300_000),
    };

    #[test]
    fn tooltip_idle() {
        assert_eq!(
            derive_tooltip(&TrayState::Idle, &NOT_PAUSED, ""),
            "Snitchwatch — filtering"
        );
    }

    #[test]
    fn tooltip_pending_uses_count() {
        assert_eq!(
            derive_tooltip(&TrayState::Pending(3), &NOT_PAUSED, ""),
            "3 pending decisions"
        );
    }

    #[test]
    fn tooltip_recent_block_includes_what() {
        let s = TrayState::RecentBlock {
            what: "spotify → tracker.x".into(),
            ttl: Duration::from_secs(3),
        };
        assert_eq!(
            derive_tooltip(&s, &NOT_PAUSED, ""),
            "Blocked: spotify → tracker.x"
        );
    }

    #[test]
    fn tooltip_filter_off() {
        // A bridge that predates `FilterPauseState` gives no end time.
        assert_eq!(
            derive_tooltip(&TrayState::FilterOff, &NOT_PAUSED, ""),
            "Snitchwatch — filtering disabled"
        );
    }

    #[test]
    fn tooltip_paused_shows_when_the_pause_ends() {
        assert_eq!(
            derive_tooltip(&TrayState::FilterOff, &PAUSED, "14:30"),
            "Snitchwatch — filtering paused until 14:30"
        );
    }

    #[test]
    fn tooltip_daemon_down() {
        assert_eq!(
            derive_tooltip(&TrayState::DaemonDown, &NOT_PAUSED, ""),
            "opensnitchd not reachable"
        );
    }

    #[test]
    fn menu_model_not_paused_offers_pause() {
        for state in [
            TrayState::Idle,
            TrayState::Pending(2),
            TrayState::RecentBlock {
                what: "x".into(),
                ttl: Duration::from_secs(5),
            },
        ] {
            assert_eq!(
                derive_menu_label(&state, &NOT_PAUSED),
                MenuLabel::PauseFiltering,
                "{state:?}"
            );
        }
    }

    #[test]
    fn menu_model_paused_offers_resume_even_over_transient_states() {
        // While paused the tray is FilterOff, except for a RecentBlock or a
        // DaemonDown override; the user must still be able to end the pause.
        for state in [
            TrayState::FilterOff,
            TrayState::RecentBlock {
                what: "x".into(),
                ttl: Duration::from_secs(5),
            },
            TrayState::DaemonDown,
        ] {
            assert_eq!(
                derive_menu_label(&state, &PAUSED),
                MenuLabel::ResumeFiltering,
                "{state:?}"
            );
        }
    }

    #[test]
    fn menu_label_filter_off_offers_resume() {
        assert_eq!(
            derive_menu_label(&TrayState::FilterOff, &NOT_PAUSED),
            MenuLabel::ResumeFiltering
        );
    }

    #[test]
    fn menu_label_daemon_down_offers_reconnect() {
        assert_eq!(
            derive_menu_label(&TrayState::DaemonDown, &NOT_PAUSED),
            MenuLabel::Reconnect
        );
    }

    #[test]
    fn set_filtering_paused_json_carries_the_duration() {
        let json = build_set_filtering_paused_json(true, Some(1800));
        assert_eq!(
            json,
            r#"{"action":"setFilteringPaused","paused":true,"durationSecs":1800}"#
        );
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed,
            ClientMessage::SetFilteringPaused {
                paused: true,
                duration_secs: Some(1800),
                sender_generation: None,
                sender_uid: None,
            }
        );
        assert_eq!(
            build_set_filtering_paused_json(false, None),
            r#"{"action":"setFilteringPaused","paused":false}"#
        );
    }

    /// A QML source's code, read at test time (so a missing file fails the
    /// test rather than the build), with `//` comments and all whitespace
    /// removed so guards match code, not prose or formatting.
    fn qml_code(name: &str) -> String {
        let path = format!("{}/qml/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .lines()
            .map(|line| line.split("//").next().unwrap_or_default())
            .flat_map(|code| code.chars().filter(|c| !c.is_whitespace()))
            .collect()
    }

    #[test]
    fn tray_menu_offers_exactly_the_allowed_pause_durations() {
        // Timed only (issue #47): no untimed toggle, and every offered
        // duration is one the bridge accepts.
        let tray_menu = qml_code("TrayMenu.qml");
        assert!(
            !tray_menu.contains("toggleFiltering"),
            "untimed pause toggle is back"
        );
        let offered: Vec<u64> = tray_menu
            .split("pauseFor(")
            .skip(1)
            .map(|rest| rest[..rest.find(')').unwrap()].trim().parse().unwrap())
            .collect();
        assert_eq!(
            offered,
            snitchwatch_bridge::filter_pause::ALLOWED_PAUSE_SECS.to_vec()
        );
    }

    #[test]
    fn tray_menu_is_flat() {
        // Plasma's StatusNotifierItem tray (dbusmenu) showed a nested
        // `Labs.Menu` as a bare "Pause filtering" entry with no submenu,
        // and activating it did nothing (VM run r6). The menu's root is its
        // only `Labs.Menu`, and the tray icon uses it rather than a menu of
        // its own. tests/tray_menu_qml.rs checks flatness at runtime too.
        assert_eq!(
            qml_code("TrayMenu.qml").matches("Labs.Menu{").count(),
            1,
            "TrayMenu.qml nests a submenu"
        );
        let main_qml = qml_code("main.qml");
        assert!(
            main_qml.contains("menu:TrayMenu{"),
            "the tray icon doesn't use TrayMenu"
        );
        assert!(
            !main_qml.contains("Labs.Menu{"),
            "main.qml declares a menu of its own"
        );
    }

    /// Issue #78: paused with prompts holding the slot, the tooltip says the
    /// pause can't reach other new connections. Otherwise it is unchanged.
    #[test]
    fn a_pause_with_a_waiting_prompt_says_so() {
        let tooltip = derive_tooltip_with_slot(&TrayState::FilterOff, &PAUSED, "14:30", 1);
        assert_eq!(
            tooltip,
            format!(
                "Snitchwatch — filtering paused until 14:30. {}",
                crate::prompt_slot_text::paused_while_waiting(1)
            )
        );
        assert_eq!(
            derive_tooltip_with_slot(&TrayState::FilterOff, &PAUSED, "", 2),
            format!(
                "Snitchwatch — filtering paused. {}",
                crate::prompt_slot_text::paused_while_waiting(2)
            )
        );
        for (state, pause, holders) in [
            (TrayState::FilterOff, PAUSED, 0),
            (TrayState::Pending(1), NOT_PAUSED, 1),
            (TrayState::DaemonDown, PAUSED, 1),
        ] {
            assert_eq!(
                derive_tooltip_with_slot(&state, &pause, "14:30", holders),
                derive_tooltip(&state, &pause, "14:30"),
                "{state:?} {holders}"
            );
        }
    }

    /// Only the live session's prompt-slot state counts: after a
    /// disconnect, or on an older bridge, nothing is known to be waiting.
    #[test]
    fn only_the_live_sessions_holders_count() {
        assert_eq!(live_slot_holders(2, true), 2);
        assert_eq!(live_slot_holders(2, false), 0);
        assert_eq!(live_slot_holders(0, true), 0);
    }
}
