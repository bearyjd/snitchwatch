//! XDG-aware path resolver.
//!
//! All Snitchwatch state lives under $XDG_STATE_HOME (logs, crash dumps),
//! $XDG_DATA_HOME (sqlite), and $XDG_CONFIG_HOME (autostart, settings).
//! Falls back to ~/.local/{state,share}/ and ~/.config/ when the env vars
//! are unset.

use std::path::PathBuf;

pub fn state_dir() -> PathBuf {
    state_dir_from(
        env_var("XDG_STATE_HOME").as_deref(),
        env_var("HOME").as_deref(),
    )
}

pub fn data_dir() -> PathBuf {
    data_dir_from(
        env_var("XDG_DATA_HOME").as_deref(),
        env_var("HOME").as_deref(),
    )
}

pub fn config_dir() -> PathBuf {
    config_dir_from(
        env_var("XDG_CONFIG_HOME").as_deref(),
        env_var("HOME").as_deref(),
    )
}

pub fn autostart_path() -> PathBuf {
    autostart_path_from(
        env_var("XDG_CONFIG_HOME").as_deref(),
        env_var("HOME").as_deref(),
    )
}

pub fn bridge_log_path() -> PathBuf {
    state_dir().join("bridge.log")
}

pub fn crash_log_path() -> PathBuf {
    state_dir().join("crash.log")
}

/// Pure core of [`state_dir`]: the env values come in as parameters so the
/// resolution rules can be tested without touching process-global state.
fn state_dir_from(xdg_state_home: Option<&str>, home: Option<&str>) -> PathBuf {
    xdg_dir_from(xdg_state_home, home, ".local/state").join("snitchwatch")
}

/// Pure core of [`data_dir`] (issue #97).
fn data_dir_from(xdg_data_home: Option<&str>, home: Option<&str>) -> PathBuf {
    xdg_dir_from(xdg_data_home, home, ".local/share").join("snitchwatch")
}

/// Pure core of [`config_dir`] (issue #97).
fn config_dir_from(xdg_config_home: Option<&str>, home: Option<&str>) -> PathBuf {
    xdg_dir_from(xdg_config_home, home, ".config").join("snitchwatch")
}

/// Pure core of [`autostart_path`].
fn autostart_path_from(xdg_config_home: Option<&str>, home: Option<&str>) -> PathBuf {
    xdg_dir_from(xdg_config_home, home, ".config")
        .join("autostart")
        .join("snitchwatch.desktop")
}

/// An unset or empty `xdg` value is treated as unset; with no `home` either,
/// resolution falls back to `/tmp`.
fn xdg_dir_from(xdg: Option<&str>, home: Option<&str>, fallback_subpath: &str) -> PathBuf {
    match xdg {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from(home.unwrap_or("/tmp")).join(fallback_subpath),
    }
}

/// The only place this module reads the process environment.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the pure `*_from` resolvers with explicit inputs. They
    // must never call `std::env::set_var`/`remove_var`: env vars are
    // process-global and `cargo test` runs tests on parallel threads, so any
    // mutation here would race every other test that resolves a path.

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
    }

    #[test]
    fn state_dir_treats_empty_xdg_as_unset() {
        assert_eq!(
            state_dir_from(Some(""), Some("/home/alice")),
            PathBuf::from("/home/alice/.local/state/snitchwatch")
        );
    }

    #[test]
    fn state_dir_falls_back_to_tmp_when_home_is_unset() {
        assert_eq!(
            state_dir_from(None, None),
            PathBuf::from("/tmp/.local/state/snitchwatch")
        );
    }

    #[test]
    fn data_dir_uses_xdg_or_falls_back_to_home_local_share() {
        assert_eq!(
            data_dir_from(Some("/x/data"), Some("/home/alice")),
            PathBuf::from("/x/data/snitchwatch")
        );
        assert_eq!(
            data_dir_from(Some(""), Some("/home/alice")),
            PathBuf::from("/home/alice/.local/share/snitchwatch")
        );
        assert_eq!(
            data_dir_from(None, None),
            PathBuf::from("/tmp/.local/share/snitchwatch")
        );
    }

    #[test]
    fn config_dir_uses_xdg_or_falls_back_to_home_config() {
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
    fn autostart_path_uses_config_dir() {
        assert_eq!(
            autostart_path_from(Some("/tmp/cfg"), Some("/home/alice")),
            PathBuf::from("/tmp/cfg/autostart/snitchwatch.desktop")
        );
    }

    #[test]
    fn autostart_path_falls_back_to_home_config() {
        assert_eq!(
            autostart_path_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.config/autostart/snitchwatch.desktop")
        );
    }
}
