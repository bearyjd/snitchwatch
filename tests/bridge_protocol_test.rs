//! End-to-end test: mock_opensnitchd (gRPC client) ↔ bridge (gRPC server +
//! WS server) ↔ WebSocket UI client.
//!
//! This is the M1.5 acceptance test. It proves that:
//!   1. The bridge binds both its gRPC and WebSocket ports.
//!   2. opensnitchd (mocked) can dial in and call `AskRule`.
//!   3. The bridge broadcasts an `InsertConnectionRows` to any connected WS
//!      client carrying the new pending row.
//!   4. A WS `setVerdict` resolves the pending row, and the original
//!      `AskRule` unary call returns a `Rule` whose `action` matches.

use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use serde_json::json;
use snitchwatch_bridge::daemon_watchdog::DAEMON_DOWN_TIMEOUT;
use snitchwatch_bridge::tray_state::TrayState;
use snitchwatch_bridge::ws_messages::ServerMessage;
use snitchwatch_bridge_cli::{run, BridgeConfig};
use snitchwatch_proto::protocol::Connection;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

#[path = "bridge_protocol_test/ask_rule.rs"]
mod ask_rule;
#[path = "bridge_protocol_test/diagnostics.rs"]
mod diagnostics;
#[path = "bridge_protocol_test/gui_lifecycle.rs"]
mod gui_lifecycle;
#[path = "bridge_protocol_test/rules.rs"]
mod rules;

/// Connect to the bridge's `/stream` route over its Unix domain socket and
/// present the handshake token as the first frame, exactly as a real client
/// (or `crate::loopback_proxy` on the Tauri shell's behalf) must.
async fn connect_stream(socket_path: &std::path::Path, token: &str) -> WebSocketStream<UnixStream> {
    let stream = UnixStream::connect(socket_path)
        .await
        .expect("unix socket connect failed");
    let (mut ws, _resp) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .expect("ws handshake failed");
    ws.send(Message::Text(token.to_string()))
        .await
        .expect("token handshake send failed");
    let ack = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = ack else {
        panic!("expected authenticated ACK")
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["action"],
        "authenticated"
    );
    ws
}

/// Next frame on the GUI's WebSocket, decoded the way a real client does.
/// The bound is generous on purpose: this suite shares the host with other
/// timing-sensitive work.
async fn next_server_message(ws: &mut WebSocketStream<UnixStream>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(&text).expect("server sent bad json");
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => panic!("ws recv error: {e}"),
                None => panic!("ws stream ended early"),
            }
        }
    })
    .await
    .expect("timed out waiting for a WS message")
}
