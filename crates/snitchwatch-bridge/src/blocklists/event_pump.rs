//! [`BlocklistEvent`] → `ServerMessage` broadcast pump, shared by the
//! production bridge (`snitchwatch-bridge-cli`) and the
//! `ws_server::serve_with_blocklists` test helper.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::blocklists::{BlocklistEvent, BlocklistsManager};
use crate::translator::downstream;
use crate::ws_messages::{ReplyTo, ServerMessage};

/// Rebroadcast every manager event as the `ServerMessage`s GUIs consume. Runs
/// until aborted. Subscribes before returning, so no event emitted after this
/// call is missed. A lagged receiver resends the full `SetBlocklists` and
/// keeps going rather than ending the pump.
pub fn spawn_event_pump(
    mgr: Arc<BlocklistsManager>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
) -> JoinHandle<()> {
    let mut events = mgr.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => publish(&mgr, &broadcast_tx, event).await,
                Err(RecvError::Lagged(skipped)) => {
                    warn!(
                        skipped,
                        "blocklist event pump lagged; resending the full list"
                    );
                    publish_set_blocklists(&mgr, &broadcast_tx).await;
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

async fn publish(
    mgr: &BlocklistsManager,
    tx: &broadcast::Sender<ServerMessage>,
    event: BlocklistEvent,
) {
    match event {
        BlocklistEvent::SubscriptionsChanged => publish_set_blocklists(mgr, tx).await,
        // Only the summary (entry count, status): entries go out a page at a
        // time, on request (issue #45).
        BlocklistEvent::EntriesChanged { .. } => publish_set_blocklists(mgr, tx).await,
        BlocklistEvent::EntriesRequested {
            subscription_id,
            offset,
            limit,
            request_id,
            reply,
        } => {
            match downstream::build_blocklist_entries_page(
                mgr,
                &subscription_id,
                offset,
                limit,
                request_id,
            )
            .await
            {
                Ok(page) => send_page(page, reply, tx).await,
                Err(e) => warn!(error = %e, ?subscription_id, "blocklist entries page failed"),
            }
        }
        BlocklistEvent::StatusChanged { subscription_id } => {
            match downstream::build_set_blocklist_status(mgr, &subscription_id).await {
                Ok(m) => {
                    let _ = tx.send(m);
                }
                Err(e) => warn!(error = %e, ?subscription_id, "blocklist status rebuild failed"),
            }
            // `SetBlocklistStatus` carries no enforcement state; the summary does.
            publish_set_blocklists(mgr, tx).await;
        }
        BlocklistEvent::SubscriptionRejected { url, reason } => {
            let _ = tx.send(downstream::build_rejected_blocklist(&url, &reason));
        }
    }
}

/// How long a page may wait for room on its connection's queue.
const PAGE_WAIT: Duration = Duration::from_secs(2);

/// A page goes to the connection that asked for it and to no other (issue
/// #67); a request with no connection is answered on the broadcast. A GUI
/// that stopped reading is waited on once, then its pages are dropped without
/// waiting, so it can't stall the pump for everyone else.
async fn send_page(
    page: ServerMessage,
    reply: Option<ReplyTo>,
    broadcast: &broadcast::Sender<ServerMessage>,
) {
    let Some(reply) = reply else {
        let _ = broadcast.send(page);
        return;
    };
    if reply.stalled() {
        // Without waiting; one that fits means the GUI reads again.
        if reply.try_send(page) {
            reply.clear_stalled();
        }
    } else if tokio::time::timeout(PAGE_WAIT, reply.send(page))
        .await
        .is_err()
    {
        warn!("a GUI isn't reading its blocklist pages; no longer waiting for it");
        reply.mark_stalled();
    }
}

async fn publish_set_blocklists(mgr: &BlocklistsManager, tx: &broadcast::Sender<ServerMessage>) {
    match downstream::build_set_blocklists(mgr).await {
        Ok(m) => {
            let _ = tx.send(m);
            let _ = tx.send(downstream::build_set_blocklist_leftovers(mgr));
        }
        Err(e) => warn!(error = %e, "blocklist summary rebuild failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocklists::store::BlocklistStore;

    fn manager() -> Arc<BlocklistsManager> {
        Arc::new(BlocklistsManager::new(Arc::new(
            BlocklistStore::open_in_memory().unwrap(),
        )))
    }

    /// The pre-#45 pump was `while let Ok(evt) = rx.recv()`: one `Lagged`
    /// ended it for good and GUIs never heard of a subscription again.
    #[tokio::test]
    async fn the_pump_survives_a_lagged_event_bus() {
        let mgr = manager();
        let (tx, mut rx) = broadcast::channel(1024);
        let pump = spawn_event_pump(mgr.clone(), tx);
        // The current-thread runtime doesn't run the pump until we yield, so
        // these overflow the 64-slot event bus.
        for i in 0..200 {
            mgr.reject_subscription(&format!("http://x.example/{i}"), "only https");
        }
        tokio::task::yield_now().await;
        let id = mgr
            .add_subscription("https://x.example/after-lag.txt")
            .await
            .unwrap();
        let seen = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let ServerMessage::SetBlocklists { blocklists, .. } = rx.recv().await.unwrap() {
                    if blocklists.iter().any(|b| b.id == id) {
                        break;
                    }
                }
            }
        })
        .await;
        pump.abort();
        assert!(seen.is_ok(), "no SetBlocklists after the event bus lagged");
    }

    #[tokio::test]
    async fn a_rejected_subscription_is_shown_as_a_refused_row() {
        let mgr = manager();
        let (tx, mut rx) = broadcast::channel(16);
        let pump = spawn_event_pump(mgr.clone(), tx);
        mgr.reject_subscription("http://x.example/hosts", "only https");
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no message")
            .unwrap();
        pump.abort();
        match msg {
            ServerMessage::SetBlocklistDetails { details } => {
                assert_eq!(details.url, "http://x.example/hosts");
                assert_eq!(details.status, "refused");
                assert_eq!(details.last_failure_reason.as_deref(), Some("only https"));
            }
            other => panic!("expected SetBlocklistDetails, got {other:?}"),
        }
    }

    fn page() -> ServerMessage {
        ServerMessage::SetBlocklistEntries {
            subscription_id: "a".into(),
            entries: Vec::new(),
            offset: 0,
            total: 0,
            request_id: None,
            last_updated_iso8601: None,
        }
    }

    /// A GUI that stops reading is waited on once; when it reads again its
    /// pages are waited on again, not dropped for the life of the connection.
    #[tokio::test(start_paused = true)]
    async fn a_gui_that_reads_again_is_no_longer_a_stalled_one() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let reply = ReplyTo::new(tx);
        let (broadcast_tx, _keep) = broadcast::channel(4);
        send_page(page(), Some(reply.clone()), &broadcast_tx).await; // fills the queue
        assert!(!reply.stalled());
        send_page(page(), Some(reply.clone()), &broadcast_tx).await; // waits, then gives up
        assert!(reply.stalled(), "nobody read the first page");

        assert!(rx.recv().await.is_some(), "the GUI reads again");
        send_page(page(), Some(reply.clone()), &broadcast_tx).await; // fits at once
        assert!(!reply.stalled(), "a page that fit ends the stall");
        assert!(rx.try_recv().is_ok());
    }
}
