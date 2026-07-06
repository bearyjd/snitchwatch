//! System tray icon for Snitchwatch.
//!
//! Owns a `tauri::tray::TrayIcon` and re-renders it on every TrayState change
//! published by the bridge. The `derive_tooltip` and `derive_menu_label`
//! functions are pure so they can be unit-tested without a Tauri app.

use snitchwatch_bridge::tray_state::TrayState;
use tokio::sync::watch;

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

#[allow(dead_code)]
pub struct Tray {
    _rx: watch::Receiver<TrayState>,
}

impl Tray {
    /// Install the tray icon. Requires a Tauri `AppHandle`.
    ///
    /// In headless test mode (no AppHandle available), use the pure functions
    /// `derive_tooltip` and `derive_menu_label` directly instead.
    pub fn install(app: &tauri::AppHandle, rx: watch::Receiver<TrayState>) -> tauri::Result<Self> {
        use tauri::tray::TrayIconBuilder;

        let _icon = TrayIconBuilder::new()
            .tooltip(derive_tooltip(&TrayState::Idle))
            .icon(app.default_window_icon().cloned().unwrap())
            .build(app)?;

        let mut rx_clone = rx.clone();
        let icon_for_task = _icon.clone();
        tokio::spawn(async move {
            loop {
                if rx_clone.changed().await.is_err() {
                    break;
                }
                let state = rx_clone.borrow().clone();
                let _ = icon_for_task.set_tooltip(Some(derive_tooltip(&state)));
                tracing::debug!(target: "snitchwatch::tray", ?state, "tray updated");
            }
        });
        Ok(Self { _rx: rx })
    }
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
}
