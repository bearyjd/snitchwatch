//! End-to-end: a real WS client connects to the bridge, subscribes to a
//! fixture blocklist, receives SetBlocklists (never an unrequested entry
//! list) and then the SetBlocklistEntries page it asks for. No network: the list comes from a test-only fetcher, because
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

    // Entries are never broadcast: only the summary arrives on its own.
    let mut list_id = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && list_id.is_none() {
        match next_message(&mut ws).await {
            Some(ServerMessage::SetBlocklists { ref blocklists, .. })
                if blocklists.iter().any(|b| b.entry_count > 0) =>
            {
                // Downloaded, but PR A installs no daemon rule: never "enforced".
                assert!(blocklists
                    .iter()
                    .all(|b| b.enforcement == ENFORCEMENT_NOT_ENFORCED));
                list_id = Some(blocklists[0].id.clone());
            }
            Some(ServerMessage::SetBlocklistEntries { .. }) => {
                panic!("entries were broadcast without a request (issue #45)")
            }
            _ => {}
        }
    }
    let list_id = list_id.expect("never received a populated SetBlocklists");

    // The inspector asks for a page.
    let request = ClientMessage::RequestBlocklistEntries {
        subscription_id: list_id.clone(),
        offset: 0,
        limit: None,
    };
    ws.send(Message::Text(serde_json::to_string(&request).unwrap()))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never received the requested SetBlocklistEntries page"
        );
        if let Some(ServerMessage::SetBlocklistEntries {
            subscription_id,
            entries,
            offset,
            total,
        }) = next_message(&mut ws).await
        {
            assert_eq!(subscription_id, list_id);
            assert_eq!(offset, 0);
            assert_eq!(total, entries.len() as u64);
            assert!(entries.iter().any(|e| e.host == "doubleclick.net"));
            break;
        }
    }
}

async fn next_message(
    ws: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
) -> Option<ServerMessage> {
    match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str(&text).ok(),
        _ => None,
    }
}
