//! First-run wizard: detect daemon presence/state.
//!
//! Flow (from design spec §First-run wizard flow):
//!   1. Try gRPC dial with 3s timeout.
//!   2. On failure, run `systemctl --user list-unit-files snitchwatch-opensnitchd.service`.
//!   3. Parse the output:
//!        - exit non-zero or empty → UnitMissing
//!        - "disabled"/"static"/"masked" with state present → UnitInactive
//!        - "enabled" → UnreachableRetrying (something else is wrong; backoff handles it)
//!
//! The Tauri webview calls `detect_daemon_state` via the `detect_daemon_state`
//! command and renders one of three onboarding screens.

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Connected,
    UnitMissing,
    UnitInactive,
    UnreachableRetrying,
}

const GRPC_DIAL_TIMEOUT: Duration = Duration::from_secs(3);

pub async fn detect_daemon_state(grpc_endpoint: &str) -> DaemonState {
    if try_grpc_dial(grpc_endpoint).await.is_ok() {
        return DaemonState::Connected;
    }
    parse_systemctl_output(&run_systemctl_list_unit_files())
}

async fn try_grpc_dial(endpoint: &str) -> Result<(), String> {
    let endpoint = endpoint.to_string();
    let dial = async move {
        let stream = tokio::net::TcpStream::connect(&endpoint)
            .await
            .map_err(|e| e.to_string())?;
        drop(stream);
        Ok::<(), String>(())
    };
    tokio::time::timeout(GRPC_DIAL_TIMEOUT, dial)
        .await
        .map_err(|_| "timeout".to_string())?
}

fn run_systemctl_list_unit_files() -> String {
    let systemctl = match which::which("systemctl") {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    let output = std::process::Command::new(systemctl)
        .args(["--user", "list-unit-files", "snitchwatch-opensnitchd.service"])
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

pub fn parse_systemctl_output(stdout: &str) -> DaemonState {
    let line = stdout
        .lines()
        .find(|l| l.contains("snitchwatch-opensnitchd.service"));
    let line = match line {
        Some(l) => l,
        None => return DaemonState::UnitMissing,
    };
    let mut tokens = line.split_whitespace();
    let _unit = tokens.next();
    let state = tokens.next().unwrap_or("");
    match state {
        "enabled" | "static" => DaemonState::UnreachableRetrying,
        "disabled" | "masked" | "indirect" => DaemonState::UnitInactive,
        _ => DaemonState::UnitMissing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_returns_unit_missing() {
        assert_eq!(parse_systemctl_output(""), DaemonState::UnitMissing);
    }

    #[test]
    fn parse_enabled_returns_unreachable_retrying() {
        let stdout = "UNIT FILE                              STATE      VENDOR PRESET\nsnitchwatch-opensnitchd.service        enabled    disabled\n\n1 unit files listed.";
        assert_eq!(
            parse_systemctl_output(stdout),
            DaemonState::UnreachableRetrying
        );
    }

    #[test]
    fn parse_disabled_returns_unit_inactive() {
        let stdout = "UNIT FILE                              STATE      VENDOR PRESET\nsnitchwatch-opensnitchd.service        disabled   disabled\n\n1 unit files listed.";
        assert_eq!(
            parse_systemctl_output(stdout),
            DaemonState::UnitInactive
        );
    }

    #[test]
    fn parse_other_unit_returns_unit_missing() {
        let stdout = "UNIT FILE         STATE      VENDOR PRESET\nsomething-else.service  enabled    disabled\n\n1 unit files listed.";
        assert_eq!(parse_systemctl_output(stdout), DaemonState::UnitMissing);
    }

    #[test]
    fn parse_masked_returns_unit_inactive() {
        let stdout = "snitchwatch-opensnitchd.service        masked     disabled";
        assert_eq!(
            parse_systemctl_output(stdout),
            DaemonState::UnitInactive
        );
    }
}
