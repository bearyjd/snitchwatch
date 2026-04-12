//! End-to-end: a real WS client connects to the bridge, subscribes to a
//! file:// blocklist, and receives SetBlocklists + SetBlocklistEntries messages.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use snitchwatch_bridge::blocklists::store::BlocklistStore;
use snitchwatch_bridge::blocklists::BlocklistsManager;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_blocklist_via_ws_yields_entries() {
    let fixture = std::env::current_dir()
        .unwrap()
        .join("../../tests/fixtures/blocklists/domains-tiny.txt")
        .canonicalize()
        .expect("fixture file must exist");
    let file_url = format!("file://{}", fixture.display());

    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    let mgr = Arc::new(BlocklistsManager::new(store));
    let (ws_url, _shutdown) =
        snitchwatch_bridge::ws_server::serve_with_blocklists("127.0.0.1:0".parse().unwrap(), mgr)
            .await
            .expect("bridge boots");

    // Brief pause to let the server start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (mut ws, _resp) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws client connects");

    // Subscribe to the file:// blocklist.
    let sub_msg = ClientMessage::SubscribeBlocklist { url: file_url };
    ws.send(Message::Text(serde_json::to_string(&sub_msg).unwrap()))
        .await
        .unwrap();

    // Collect messages until we see a populated SetBlocklists and SetBlocklistEntries.
    let mut saw_set = false;
    let mut saw_entries = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !(saw_set && saw_entries) {
        let read = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
        let msg = match read {
            Ok(Some(Ok(Message::Text(text)))) => {
                match serde_json::from_str::<ServerMessage>(&text) {
                    Ok(m) => m,
                    Err(_) => continue,
                }
            }
            _ => continue,
        };
        match msg {
            ServerMessage::SetBlocklists { ref blocklists }
                if blocklists.iter().any(|b| b.entry_count > 0) =>
            {
                saw_set = true;
            }
            ServerMessage::SetBlocklistEntries { ref entries, .. } if !entries.is_empty() => {
                assert!(entries.iter().any(|e| e.host == "doubleclick.net"));
                saw_entries = true;
            }
            _ => {}
        }
    }
    assert!(saw_set, "never received populated SetBlocklists");
    assert!(saw_entries, "never received SetBlocklistEntries");
}
