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
    autostart_path_from(xdg("XDG_CONFIG_HOME").as_deref(), home().as_deref())
}

/// Per-user autostart entry for upstream `opensnitch-ui`, if installed —
/// see `crate::coexistence` and README.md's "Coexistence with upstream
/// opensnitch-ui" section. Same directory as our own `autostart_path()`,
/// just the upstream project's filename.
pub fn opensnitch_ui_autostart_path() -> PathBuf {
    opensnitch_ui_autostart_path_from(xdg("XDG_CONFIG_HOME").as_deref(), home().as_deref())
}

/// Persisted preferences (`crate::settings`), e.g. the RDAP opt-in flag.
pub fn settings_path() -> PathBuf {
    settings_path_from(xdg("XDG_CONFIG_HOME").as_deref(), home().as_deref())
}

pub fn bridge_log_path() -> PathBuf {
    state_dir().join("bridge.log")
}

pub fn crash_log_path() -> PathBuf {
    state_dir().join("crash.log")
}

/// The GUI's own log, written when `SNITCHWATCH_LOG` is set
/// (`crate::logging`).
pub fn gui_log_path() -> PathBuf {
    state_dir().join("gui.log")
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

fn autostart_path_from(xdg_config_home: Option<&str>, home: Option<&str>) -> PathBuf {
    autostart_dir_from(xdg_config_home, home).join("snitchwatch.desktop")
}

fn opensnitch_ui_autostart_path_from(xdg_config_home: Option<&str>, home: Option<&str>) -> PathBuf {
    autostart_dir_from(xdg_config_home, home).join("opensnitch_ui.desktop")
}

fn settings_path_from(xdg_config_home: Option<&str>, home: Option<&str>) -> PathBuf {
    config_dir_from(xdg_config_home, home).join("settings.json")
}

/// An unset or empty `xdg` value is treated as unset; with no `home` either,
/// resolution falls back to `/tmp`. (The process-reading `home()` never gets
/// there: it supplies a per-user root instead.)
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

/// Where nothing can be created or read: the no-`HOME` base when its root
/// can't be trusted, so no setting is read from or written into it.
const REFUSED_BASE: &str = "/proc/snitchwatch-no-trusted-home";

/// `HOME`, or with it unset or empty a per-user root under `/tmp`. That root
/// is checked once, here, before any path under it is handed out (settings
/// and autostart too, not only the logs): if it isn't a real directory of
/// ours, every path points somewhere that can't exist, so settings read as
/// defaults and can't be written (#116 review M1).
fn home() -> Option<String> {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => Some(home),
        _ => Some(no_home_base()),
    }
}

fn no_home_base() -> String {
    static BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BASE.get_or_init(|| {
        let root = crate::private_files::no_home_root();
        resolve_no_home(&root, crate::private_files::ensure_private_root(&root))
    })
    .clone()
}

fn resolve_no_home(root: &std::path::Path, checked: std::io::Result<()>) -> String {
    match checked {
        Ok(()) => root.to_string_lossy().into_owned(),
        Err(error) => {
            tracing::warn!(%error, root = %root.display(),
                "HOME is unset and the per-user fallback directory isn't ours; \
                 settings are not read or saved");
            REFUSED_BASE.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These exercise the pure `*_from` resolvers with explicit inputs. They
    // must never call `std::env::set_var`/`remove_var`: env vars are
    // process-global and `cargo test` runs tests on parallel threads.

    /// #116 review M1: a refused no-HOME root hands out paths that can't be
    /// read or written, and settings fall back to defaults.
    #[test]
    fn an_untrusted_no_home_root_leaves_settings_unread_and_unwritten() {
        let (foreign, _guard) = crate::private_files::foreign_owned_dir();
        let root = foreign.as_path();
        let checked = crate::private_files::ensure_private_root(root);
        let base = resolve_no_home(root, checked);
        assert_eq!(base, REFUSED_BASE);
        let path = settings_path_from(None, Some(&base));
        assert!(path.starts_with(REFUSED_BASE));
        assert_eq!(
            crate::settings::read_settings(&path),
            crate::settings::Settings::default()
        );
        assert!(crate::settings::write_settings(&path, &Default::default()).is_err());
        assert!(!std::path::Path::new(REFUSED_BASE).exists());
    }

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
        assert_eq!(
            autostart_dir_from(Some("/tmp/cfg"), Some("/home/alice")),
            PathBuf::from("/tmp/cfg/autostart")
        );
        assert_eq!(
            autostart_dir_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.config/autostart")
        );
    }

    #[test]
    fn autostart_path_names_our_desktop_file() {
        assert_eq!(
            autostart_path_from(Some("/tmp/cfg"), Some("/home/alice")),
            PathBuf::from("/tmp/cfg/autostart/snitchwatch.desktop")
        );
        assert_eq!(
            autostart_path_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.config/autostart/snitchwatch.desktop")
        );
    }

    #[test]
    fn opensnitch_ui_autostart_path_names_the_upstream_desktop_file() {
        assert_eq!(
            opensnitch_ui_autostart_path_from(Some("/tmp/cfg"), Some("/home/alice")),
            PathBuf::from("/tmp/cfg/autostart/opensnitch_ui.desktop")
        );
        assert_eq!(
            opensnitch_ui_autostart_path_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.config/autostart/opensnitch_ui.desktop")
        );
    }

    #[test]
    fn settings_path_uses_config_dir() {
        assert_eq!(
            settings_path_from(Some("/tmp/cfg"), None),
            PathBuf::from("/tmp/cfg/snitchwatch/settings.json")
        );
        assert_eq!(
            settings_path_from(None, Some("/home/alice")),
            PathBuf::from("/home/alice/.config/snitchwatch/settings.json")
        );
    }
}
