use super::*;

fn prepare() -> ClientMessage {
    serde_json::from_value(serde_json::json!({"action":"prepareCuratedBroadOptIn","requestId":"dns-1-1","id":"dns-resolved","purpose":"enable"})).unwrap()
}
#[tokio::test]
async fn modern_queue_requires_current_capable_origin_and_never_replays() {
    let (broadcast_tx, _) = broadcast::channel(8);
    let (inbound_tx, mut rx) = mpsc::channel(1);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
    };
    let session = mark_connected(&connection, false);
    assert!(handles.try_send_for_session(session, prepare()).is_err());
    connection.lock().unwrap().curated_broad_opt_in = true;
    assert!(
        handles.try_send(prepare()).is_err(),
        "generic send assigned modern intent a session"
    );
    assert_eq!(handles.try_send_for_session(session, prepare()), Ok(()));
    assert_eq!(
        handles.try_send_for_session(session, prepare()),
        Err(SendClientMessageError::Full)
    );
    disconnect_and_discard(&connection, &mut rx);
    assert!(rx.try_recv().is_err(), "queued consent survived disconnect");
    let next = mark_connected(&connection, false);
    connection.lock().unwrap().curated_broad_opt_in = true;
    assert!(!handles.advertises_curated_broad_opt_in(session));
    assert!(handles.advertises_curated_broad_opt_in(next));
    assert_eq!(
        handles.try_send_for_session(session, prepare()),
        Err(SendClientMessageError::StaleSession)
    );
}
