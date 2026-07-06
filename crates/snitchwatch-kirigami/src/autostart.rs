//! XDG autostart `.desktop` file management (Task 15).
//!
//! Ported from `snitchwatch-tauri::commands`'s `read_autostart_state` /
//! `write_autostart_desktop` / `remove_autostart_desktop` — pure `std::fs`
//! logic with zero Tauri dependency (the Tauri shell only used
//! `tauri-plugin-autostart` for macOS/Windows launch-agent parity; on Linux
//! it's always been a plain XDG `.desktop` file). No plugin needed here:
//! `settings_controller.rs` calls these directly from a background thread so
//! the Qt thread never blocks on disk I/O.

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct AutostartState {
    pub enabled: bool,
    pub path: PathBuf,
}

pub fn read_autostart_state(path: &Path) -> AutostartState {
    AutostartState {
        enabled: path.exists(),
        path: path.to_path_buf(),
    }
}

pub fn write_autostart_desktop(path: &Path, exec: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = format!(
        "[Desktop Entry]\nType=Application\nName=Snitchwatch\nExec={exec}\nIcon=security-high\nX-GNOME-Autostart-enabled=true\nNoDisplay=false\n"
    );
    std::fs::write(path, body)
}

pub fn remove_autostart_desktop(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_autostart_state_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("snitchwatch.desktop");
        std::fs::write(&p, "stub").unwrap();
        let state = read_autostart_state(&p);
        assert!(state.enabled);
        assert_eq!(state.path, p);
    }

    #[test]
    fn read_autostart_state_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("snitchwatch.desktop");
        let state = read_autostart_state(&p);
        assert!(!state.enabled);
    }

    #[test]
    fn write_then_read_autostart_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("snitchwatch.desktop");
        write_autostart_desktop(&p, "/usr/bin/snitchwatch-kirigami").unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("[Desktop Entry]"));
        assert!(body.contains("Exec=/usr/bin/snitchwatch-kirigami"));
        assert!(read_autostart_state(&p).enabled);
    }

    #[test]
    fn remove_autostart_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("snitchwatch.desktop");
        remove_autostart_desktop(&p).unwrap();
        std::fs::write(&p, "stub").unwrap();
        remove_autostart_desktop(&p).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn write_autostart_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("snitchwatch.desktop");
        write_autostart_desktop(&p, "/usr/bin/old").unwrap();
        write_autostart_desktop(&p, "/usr/bin/new").unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("Exec=/usr/bin/new"));
        assert!(!body.contains("/usr/bin/old"));
    }
}
