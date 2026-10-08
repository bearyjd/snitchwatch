//! End-to-end: a real WS client connects to the bridge, subscribes to a
//! fixture blocklist, and receives SetBlocklists + SetBlocklistEntries
//! messages. No network: the list comes from a test-only fetcher, because
//! production fetches only `https` and has no file-reading path (issue #45).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use snitchwatch_bridge::blocklists::fetcher::{process_body, BlocklistFetch, FetchOutcome};
use snitchwatch_bridge::blocklists::store::BlocklistStore;
use snitchwatch_bridge::blocklists::BlocklistsManager;
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage, ENFORCEMENT_NOT_ENFORCED};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;

const FIXTURE_PREFIX: &str = "https://fixtures.invalid/";
const FIXTURE_MAX_BYTES: u64 = 1024 * 1024;

/// Serves `tests/fixtures/blocklists/<name>` for `https://fixtures.invalid/<name>`
/// (size-capped) and fails every other URL.
struct FixtureFetcher;

#[async_trait::async_trait]
impl BlocklistFetch for FixtureFetcher {
    async fn fetch(&self, url: &str) -> FetchOutcome {
        let Some(name) = url.strip_prefix(FIXTURE_PREFIX) else {
            return FetchOutcome::Failed {
                reason: format!("not a fixture URL: {url}"),
            };
        };
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/blocklists")
            .join(name);
        let body = std::fs::metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                if m.len() <= FIXTURE_MAX_BYTES {
                    std::fs::read_to_string(&path).map_err(|e| e.to_string())
                } else {
                    Err(format!("fixture too large: {} bytes", m.len()))
                }
            });
        match body {
            Ok(body) => process_body(&body),
            Err(reason) => FetchOutcome::Failed { reason },
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_blocklist_via_ws_yields_entries() {
    let fixture_url = format!("{FIXTURE_PREFIX}domains-tiny.txt");

    let store = Arc::new(BlocklistStore::open_in_memory().unwrap());
    let mgr = Arc::new(BlocklistsManager::new(store).with_fetcher(Arc::new(FixtureFetcher)));
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("bridge.sock");
    let (socket_path, token, _shutdown) =
        snitchwatch_bridge::ws_server::serve_with_blocklists(socket_path, mgr)
            .await
            .expect("bridge boots");

    // Brief pause to let the server start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("unix socket connect failed");
    let (mut ws, _resp) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .expect("ws client connects");

    // Present the handshake token before anything else.
    ws.send(Message::Text(token.as_str().to_string()))
        .await
        .expect("token handshake send failed");

    // Subscribe to the fixture blocklist.
    let sub_msg = ClientMessage::SubscribeBlocklist { url: fixture_url };
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
            ServerMessage::SetBlocklists { ref blocklists, .. }
                if blocklists.iter().any(|b| b.entry_count > 0) =>
            {
                // Downloaded, but PR A installs no daemon rule: never "enforced".
                assert!(blocklists
                    .iter()
                    .all(|b| b.enforcement == ENFORCEMENT_NOT_ENFORCED));
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
