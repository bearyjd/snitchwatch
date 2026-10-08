//! XDG-aware path resolver.
//!
//! All Snitchwatch state lives under $XDG_STATE_HOME (logs, crash dumps),
//! $XDG_DATA_HOME (sqlite), and $XDG_CONFIG_HOME (autostart, settings).
//! Falls back to ~/.local/{state,share}/ and ~/.config/ when the env vars
//! are unset.
//!
//! Ported verbatim from `snitchwatch-tauri::paths` (Task 14): pure `std`, no
//! Tauri dependency — the XDG resolution is toolkit-agnostic.

use std::path::PathBuf;

pub fn state_dir() -> PathBuf {
    state_dir_from(xdg("XDG_STATE_HOME").as_deref(), home().as_deref())
}

pub fn data_dir() -> PathBuf {
    data_dir_from(xdg("XDG_DATA_HOME").as_deref(), home().as_deref())
}

pub fn config_dir() -> PathBuf {
    config_dir_from(xdg("XDG_CONFIG_HOME").as_deref(), home().as_deref())
}

pub fn autostart_path() -> PathBuf {
    autostart_dir_from(xdg("XDG_CONFIG_HOME").as_deref(), home().as_deref())
        .join("snitchwatch.desktop")
}

/// Per-user autostart entry for upstream `opensnitch-ui`, if installed —
/// see `crate::coexistence` and README.md's "Coexistence with upstream
/// opensnitch-ui" section. Same directory as our own `autostart_path()`,
/// just the upstream project's filename.
pub fn opensnitch_ui_autostart_path() -> PathBuf {
    autostart_dir_from(xdg("XDG_CONFIG_HOME").as_deref(), home().as_deref())
        .join("opensnitch_ui.desktop")
}

/// Persisted preferences (`crate::settings`), e.g. the RDAP opt-in flag.
pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn bridge_log_path() -> PathBuf {
    state_dir().join("bridge.log")
}

pub fn crash_log_path() -> PathBuf {
    state_dir().join("crash.log")
}

// Pure cores of the resolvers above: the environment's values come in as
// parameters, so tests never touch process-global state (issues #96, #97).

fn state_dir_from(xdg_state_home: Option<&str>, home: Option<&str>) -> PathBuf {
    xdg_dir_from(xdg_state_home, home, ".local/state").join("snitchwatch")
}

fn data_dir_from(xdg_data_home: Option<&str>, home: Option<&str>) -> PathBuf {
    xdg_dir_from(xdg_data_home, home, ".local/share").join("snitchwatch")
}

fn config_dir_from(xdg_config_home: Option<&str>, home: Option<&str>) -> PathBuf {
    xdg_dir_from(xdg_config_home, home, ".config").join("snitchwatch")
}

fn autostart_dir_from(xdg_config_home: Option<&str>, home: Option<&str>) -> PathBuf {
    xdg_dir_from(xdg_config_home, home, ".config").join("autostart")
}

/// An unset or empty `xdg` value is treated as unset; with no `home` either,
/// resolution falls back to `/tmp`.
fn xdg_dir_from(xdg: Option<&str>, home: Option<&str>, fallback_subpath: &str) -> PathBuf {
    match xdg {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(home.unwrap_or("/tmp")).join(fallback_subpath),
    }
}

/// The only places this module reads the process environment.
fn xdg(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn home() -> Option<String> {
    std::env::var("HOME").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the pure `*_from` resolvers with explicit inputs. They
    // must never call `std::env::set_var`/`remove_var`: env vars are
    // process-global and `cargo test` runs tests on parallel threads.

    #[test]
    fn state_dir_uses_xdg_when_set() {
        assert_eq!(
            state_dir_from(Some("/tmp/snitchwatch-test-state"), Some("/home/alice")),
            PathBuf::from("/tmp/snitchwatch-test-state/snitchwatch")
        );
    }

    #[test]
    fn state_dir_falls_back_to_home_local_state() {
        assert_eq!(
            state_dir_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.local/state/snitchwatch")
        );
        assert_eq!(
            state_dir_from(Some(""), Some("/home/alice")),
            PathBuf::from("/home/alice/.local/state/snitchwatch"),
            "an empty value counts as unset"
        );
        assert_eq!(
            state_dir_from(None, None),
            PathBuf::from("/tmp/.local/state/snitchwatch")
        );
    }

    #[test]
    fn data_and_config_dirs_follow_their_xdg_vars() {
        assert_eq!(
            data_dir_from(Some("/x/data"), Some("/home/alice")),
            PathBuf::from("/x/data/snitchwatch")
        );
        assert_eq!(
            data_dir_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.local/share/snitchwatch")
        );
        assert_eq!(
            config_dir_from(Some("/x/cfg"), Some("/home/alice")),
            PathBuf::from("/x/cfg/snitchwatch")
        );
        assert_eq!(
            config_dir_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.config/snitchwatch")
        );
    }

    #[test]
    fn autostart_entries_use_the_config_dir() {
        let dir = autostart_dir_from(Some("/tmp/cfg"), Some("/home/alice"));
        assert_eq!(dir, PathBuf::from("/tmp/cfg/autostart"));
        assert_eq!(
            autostart_dir_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.config/autostart")
        );
    }

    #[test]
    fn settings_path_uses_config_dir() {
        assert_eq!(
            config_dir_from(Some("/tmp/cfg"), None).join("settings.json"),
            PathBuf::from("/tmp/cfg/snitchwatch/settings.json")
        );
    }
}
