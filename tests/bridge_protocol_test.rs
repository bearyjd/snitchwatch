//! End-to-end test: mock_opensnitchd ↔ bridge ↔ WebSocket client.
//!
//! This is the M1 acceptance test scaffold. It proves that:
//!   1. The bridge can connect to a gRPC server (done in this cut)
//!   2. AskRule notifications produce pending WS rows (Task 19+)
//!   3. WS verdicts close the loop and the mock observes the reply (Task 19+)
//!
//! Plan 1's first cut verifies only that the mock spawns and accepts a gRPC
//! connection. The full round-trip becomes the success criterion after the
//! bridge CLI orchestrator lands in Task 19.

use mock_opensnitchd::{MockOpensnitchd, ScriptedEvent};
use snitchwatch_proto::protocol::Notification;

#[tokio::test]
async fn mock_accepts_tonic_client_connection() {
    let _ = tracing_subscriber::fmt::try_init();

    let mock = MockOpensnitchd::new();
    let ask = Notification::default();
    mock.script(vec![ScriptedEvent::Notification(ask)]).await;
    let mock_addr = mock.spawn().await;

    let endpoint = format!("http://{}", mock_addr);
    let result = tonic::transport::Endpoint::from_shared(endpoint)
        .unwrap()
        .connect()
        .await;
    assert!(
        result.is_ok(),
        "mock should accept a tonic client connection"
    );
}
