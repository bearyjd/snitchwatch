//! XDG-aware path resolver.
//!
//! All Snitchwatch state lives under $XDG_STATE_HOME (logs, crash dumps),
//! $XDG_DATA_HOME (sqlite), and $XDG_CONFIG_HOME (autostart, settings).
//! Falls back to ~/.local/{state,share}/ and ~/.config/ when the env vars
//! are unset.

use std::path::PathBuf;

pub fn state_dir() -> PathBuf {
    xdg_dir_or("XDG_STATE_HOME", ".local/state").join("snitchwatch")
}

pub fn data_dir() -> PathBuf {
    xdg_dir_or("XDG_DATA_HOME", ".local/share").join("snitchwatch")
}

pub fn config_dir() -> PathBuf {
    xdg_dir_or("XDG_CONFIG_HOME", ".config").join("snitchwatch")
}

pub fn autostart_path() -> PathBuf {
    xdg_dir_or("XDG_CONFIG_HOME", ".config")
        .join("autostart")
        .join("snitchwatch.desktop")
}

pub fn bridge_log_path() -> PathBuf {
    state_dir().join("bridge.log")
}

pub fn crash_log_path() -> PathBuf {
    state_dir().join("crash.log")
}

fn xdg_dir_or(env_var: &str, fallback_subpath: &str) -> PathBuf {
    if let Ok(p) = std::env::var(env_var) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(fallback_subpath)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_uses_xdg_when_set() {
        std::env::set_var("XDG_STATE_HOME", "/tmp/snitchwatch-test-state");
        assert_eq!(
            state_dir(),
            PathBuf::from("/tmp/snitchwatch-test-state/snitchwatch")
        );
        std::env::remove_var("XDG_STATE_HOME");
    }

    #[test]
    fn state_dir_falls_back_to_home_local_state() {
        std::env::remove_var("XDG_STATE_HOME");
        std::env::set_var("HOME", "/home/alice");
        assert_eq!(
            state_dir(),
            PathBuf::from("/home/alice/.local/state/snitchwatch")
        );
    }

    #[test]
    fn autostart_path_uses_config_dir() {
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/cfg");
        assert_eq!(
            autostart_path(),
            PathBuf::from("/tmp/cfg/autostart/snitchwatch.desktop")
        );
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
