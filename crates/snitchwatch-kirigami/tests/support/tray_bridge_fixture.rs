//! Fixture bridge service for the tray QML tests: one authenticated session
//! that answers the tray's snapshot request, no live daemon.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use snitchwatch_bridge::diagnostics::FIREWALL_NOT_RUNNING_TROUBLESHOOTING;
use snitchwatch_bridge::tray_state::TrayState;
use snitchwatch_bridge::ws_messages::{
    CheckKind, CheckStatus, ClientMessage, DiagnosticCheck, ServerMessage,
};
use tokio::net::UnixListener;
use tokio_tungstenite::tungstenite::Message;

pub const TOKEN: &str = "tray-lifecycle-token";

pub fn report(firewall: CheckStatus) -> String {
    serde_json::to_string(&ServerMessage::DiagnosticsReport {
        checks: vec![
            DiagnosticCheck {
                kind: CheckKind::DaemonReachable,
                status: CheckStatus::Ok,
            },
            DiagnosticCheck {
                kind: CheckKind::FirewallRunning,
                status: firewall,
            },
        ],
    })
    .unwrap()
}

pub fn firewall_off() -> CheckStatus {
    CheckStatus::Failed {
        detail: FIREWALL_NOT_RUNNING_TROUBLESHOOTING.to_string(),
    }
}

/// One authenticated session: answers every `RequestSnapshot` with an idle
/// tray, no pause and the current firewall status, as the bridge answers
/// each one. The status starts as `first`; `later` (if any) is sent once,
/// after a pause, and is current from then on. Closes after `hold`. Counts every
/// `RecheckDiagnostics`.
pub async fn serve_session(
    listener: &UnixListener,
    first: CheckStatus,
    later: Option<CheckStatus>,
    hold: Duration,
    rechecks: &AtomicUsize,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
    assert!(matches!(ws.next().await, Some(Ok(Message::Text(t))) if t == TOKEN));
    ws.send(Message::Text(
        r#"{"action":"authenticated","capabilities":[]}"#.into(),
    ))
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + hold;
    let mut later = later;
    let mut current = first;
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next()).await;
        let Ok(Some(Ok(Message::Text(text)))) = next else {
            break;
        };
        match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::RecheckDiagnostics) => {
                rechecks.fetch_add(1, Ordering::SeqCst);
            }
            Ok(ClientMessage::RequestSnapshot) => {
                for message in [
                    serde_json::to_string(&ServerMessage::TrayState {
                        state: TrayState::Idle,
                    })
                    .unwrap(),
                    serde_json::to_string(&ServerMessage::FilterPauseState {
                        paused: false,
                        expires_at_unix_ms: None,
                    })
                    .unwrap(),
                    report(current.clone()),
                ] {
                    ws.send(Message::Text(message)).await.unwrap();
                }
                if let Some(status) = later.take() {
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    ws.send(Message::Text(report(status.clone())))
                        .await
                        .unwrap();
                    current = status;
                }
            }
            _ => {}
        }
    }
    let _ = ws.close(None).await;
}
