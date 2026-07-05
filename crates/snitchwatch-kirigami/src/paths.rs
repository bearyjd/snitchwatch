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
    use std::sync::Mutex;

    // Env vars are process-global; `cargo test` runs tests in one process on
    // multiple threads, so any test that mutates $XDG_*/$HOME races every other
    // one that reads them. These tests also run inside a real user session whose
    // ambient $XDG_STATE_HOME etc. are already set. Serialize env-mutating tests
    // under one lock and save/restore the vars they touch so the suite is
    // deterministic regardless of the ambient environment or parallelism.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }

        fn unset(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn state_dir_uses_xdg_when_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvVarGuard::set("XDG_STATE_HOME", "/tmp/snitchwatch-test-state");
        assert_eq!(
            state_dir(),
            PathBuf::from("/tmp/snitchwatch-test-state/snitchwatch")
        );
    }

    #[test]
    fn state_dir_falls_back_to_home_local_state() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g_xdg = EnvVarGuard::unset("XDG_STATE_HOME");
        let _g_home = EnvVarGuard::set("HOME", "/home/alice");
        assert_eq!(
            state_dir(),
            PathBuf::from("/home/alice/.local/state/snitchwatch")
        );
    }

    #[test]
    fn autostart_path_uses_config_dir() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvVarGuard::set("XDG_CONFIG_HOME", "/tmp/cfg");
        assert_eq!(
            autostart_path(),
            PathBuf::from("/tmp/cfg/autostart/snitchwatch.desktop")
        );
    }
}
