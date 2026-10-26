//! A bridge link for tests of code that sits on `BridgeHandles`: real
//! channels, no WebSocket. The tests own the "bridge" end.

use super::*;

pub(crate) struct TestLink {
    pub handles: BridgeHandles,
    pub feed: broadcast::Sender<ReceivedServerMessage>,
    pub queued: mpsc::Receiver<ClientMessage>,
    connection: Arc<Mutex<ConnectionState>>,
}

impl TestLink {
    /// `queue` is the send queue's size; `feed` the broadcast's.
    pub fn new(runtime: Handle, queue: usize, feed: usize) -> Self {
        let (broadcast_tx, _) = broadcast::channel(feed);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(queue);
        let (queued_tx, queued) = mpsc::channel(64);
        // Hand each queued message to the test as a bare `ClientMessage`,
        // while keeping the queue's real capacity.
        let connection = Arc::new(Mutex::new(ConnectionState {
            connected: true,
            connection_id: 1,
            ..ConnectionState::default()
        }));
        runtime.spawn(async move {
            while let Some(QueuedClientMessage { message, .. }) = inbound_rx.recv().await {
                if queued_tx.send(message).await.is_err() {
                    break;
                }
            }
        });
        Self {
            handles: BridgeHandles {
                broadcast_tx: broadcast_tx.clone(),
                inbound_tx,
                runtime,
                connection: connection.clone(),
                held_back: HeldBackNotices::default(),
            },
            feed: broadcast_tx,
            queued,
            connection,
        }
    }

    pub fn set_session(&self, connected: bool, connection_id: u64) {
        let mut state = self.connection.lock().expect("lock");
        state.connected = connected;
        state.connection_id = connection_id;
    }

    pub fn set_analysis_export(&self, capable: bool) {
        self.connection.lock().expect("lock").analysis_export = capable;
    }
}
