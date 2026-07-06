//! Tauri command surface for the webview.
//!
//! Each command is a thin wrapper around a pure helper that can be tested
//! without launching Tauri. The actual `#[tauri::command]` annotations are at
//! the bottom; the bodies just delegate.

use crate::paths::{autostart_path, crash_log_path};
use crate::wizard::{detect_daemon_state, DaemonState};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, PartialEq, Eq)]
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
        "[Desktop Entry]\nType=Application\nName=Snitchwatch\nExec={exec}\nIcon=snitchwatch\nX-GNOME-Autostart-enabled=true\nNoDisplay=false\n"
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

#[tauri::command]
pub async fn detect_daemon_state_cmd(grpc_endpoint: String) -> DaemonState {
    detect_daemon_state(&grpc_endpoint).await
}

#[tauri::command]
pub fn get_autostart_state() -> AutostartState {
    read_autostart_state(&autostart_path())
}

#[tauri::command]
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let path = autostart_path();
    if enabled {
        write_autostart_desktop(&path, "snitchwatch-tauri").map_err(|e| e.to_string())
    } else {
        remove_autostart_desktop(&path).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn open_crash_log() -> Result<String, String> {
    let path = crash_log_path();
    std::fs::read_to_string(&path)
        .map(|s| {
            s.lines()
                .rev()
                .take(200)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn install_daemon_stub() -> Result<String, String> {
    Err("install.sh wiring is implemented in Plan 6 (M5 packaging).".into())
}

#[tauri::command]
pub fn start_daemon_unit() -> Result<(), String> {
    let systemctl = which::which("systemctl").map_err(|e| e.to_string())?;
    let status = std::process::Command::new(systemctl)
        .args(["--user", "start", "snitchwatch-opensnitchd.service"])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("systemctl exited with {status}"))
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
        write_autostart_desktop(&p, "/usr/bin/snitchwatch-tauri").unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("[Desktop Entry]"));
        assert!(body.contains("Exec=/usr/bin/snitchwatch-tauri"));
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
}
