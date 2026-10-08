//! Onboarding observes the GUI's authenticated bridge connection. The root
//! daemon's health is reported separately by the bridge diagnostics feed;
//! desktop processes must never probe its private gRPC socket.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Connected,
    UnitMissing,
    UnitInactive,
    UnreachableRetrying,
}

pub fn state_from_authenticated_bridge(connected: bool) -> DaemonState {
    if connected {
        DaemonState::Connected
    } else {
        DaemonState::UnreachableRetrying
    }
}

/// System service management uses the normal systemd/polkit authorization.
/// Only an explicit user action invokes this, never a connection retry.
pub fn start_unit_via_systemctl() -> Result<(), String> {
    let systemctl = which::which("systemctl").map_err(|e| e.to_string())?;
    start_unit_with(&systemctl)
}

fn start_unit_with(systemctl: &Path) -> Result<(), String> {
    let output = std::process::Command::new(systemctl)
        .args(["--system", "start", "opensnitch.service"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Could not start the system OpenSnitch service: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn authenticated_bridge_is_connected_and_loss_remains_retryable() {
        assert_eq!(
            state_from_authenticated_bridge(true),
            DaemonState::Connected
        );
        assert_eq!(
            state_from_authenticated_bridge(false),
            DaemonState::UnreachableRetrying
        );
    }

    #[test]
    fn start_uses_system_daemon_and_reports_authorization_errors() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("systemctl");
        std::fs::write(&helper, "#!/bin/sh\n[ \"$#\" = 3 ] && [ \"$1\" = --system ] && [ \"$2\" = start ] && [ \"$3\" = opensnitch.service ]\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(start_unit_with(&helper).is_ok());
        std::fs::write(
            &helper,
            "#!/bin/sh\necho 'Authorization denied' >&2\nexit 1\n",
        )
        .unwrap();
        assert!(start_unit_with(&helper)
            .unwrap_err()
            .contains("Authorization denied"));
    }
}
