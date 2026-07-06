//! Persisted user preferences (currently just the RDAP opt-in toggle).
//!
//! Pure `std::fs` + `serde_json` logic, no Qt/bridge dependency — mirrors
//! `crate::autostart`'s split between plain-Rust read/write functions and
//! the cxx-qt controller (`settings_controller.rs`) that calls them off the
//! Qt thread. Stored as a small JSON file rather than a presence-only flag
//! (as autostart's `.desktop` file is) since this is expected to grow more
//! than one preference.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Persisted preferences. `#[serde(default)]` on every field means an older
/// or partially-written file still parses — missing fields fall back to
/// their default rather than failing the whole read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Whether the pending-decision insight panel is allowed to query
    /// `rdap.org` for registration info. Opt-in, default off: a firewall
    /// app must not send every pending connection's remote IP to a
    /// third-party service unless the user has explicitly asked for it.
    /// Reverse DNS (via the system resolver) is unaffected by this flag.
    #[serde(default)]
    pub rdap_enabled: bool,
}

/// Read persisted settings, degrading to [`Settings::default`] on any
/// failure (missing file, unreadable, malformed JSON) — a corrupt or absent
/// settings file must never crash or block startup.
pub fn read_settings(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

/// Persist `settings` as pretty-printed JSON, creating the parent directory
/// if needed (mirrors `autostart::write_autostart_desktop`).
pub fn write_settings(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(settings)
        .unwrap_or_else(|_| "{\"rdap_enabled\":false}".to_string());
    std::fs::write(path, body)
}

/// Convenience read for the one flag `PendingInsight::lookup` needs, without
/// callers having to know the full [`Settings`] shape.
pub fn read_rdap_enabled(path: &Path) -> bool {
    read_settings(path).rdap_enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_settings_defaults_to_rdap_disabled_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        assert_eq!(read_settings(&p), Settings::default());
        assert!(!read_rdap_enabled(&p));
    }

    #[test]
    fn read_settings_defaults_to_rdap_disabled_when_file_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(read_settings(&p), Settings::default());
    }

    #[test]
    fn write_then_read_settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("settings.json");
        write_settings(&p, &Settings { rdap_enabled: true }).unwrap();
        assert!(read_rdap_enabled(&p));
    }

    #[test]
    fn write_settings_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write_settings(&p, &Settings { rdap_enabled: true }).unwrap();
        write_settings(
            &p,
            &Settings {
                rdap_enabled: false,
            },
        )
        .unwrap();
        assert!(!read_rdap_enabled(&p));
    }

    #[test]
    fn read_settings_tolerates_a_partial_json_object() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, "{}").unwrap();
        assert_eq!(read_settings(&p), Settings::default());
    }
}
