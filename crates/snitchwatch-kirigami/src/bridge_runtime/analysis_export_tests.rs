use super::*;

fn export(cancel: bool) -> ClientMessage {
    if cancel {
        ClientMessage::CancelCacheSummaryExport {
            request_id: "gui-1".into(),
            schema_version: 1,
            reply: None,
            session: None,
        }
    } else {
        ClientMessage::RequestCacheSummaryExport {
            request_id: "gui-1".into(),
            schema_version: 1,
            filters: Default::default(),
            reply: None,
            session: None,
        }
    }
}

#[tokio::test]
async fn generic_export_cannot_acquire_an_unreviewed_session() {
    let (broadcast_tx, _) = broadcast::channel(8);
    let (inbound_tx, mut rx) = mpsc::channel(8);
    let connection = Arc::new(Mutex::new(ConnectionState::default()));
    let handles = BridgeHandles {
        broadcast_tx,
        inbound_tx,
        runtime: Handle::current(),
        connection: connection.clone(),
        held_back: HeldBackNotices::default(),
    };
    let session = mark_connected(&connection, false);
    for cancel in [false, true] {
        assert!(
            handles.try_send(export(cancel)).is_err(),
            "generic export acquired a session"
        );
        assert!(
            handles
                .try_send_for_session(session, export(cancel))
                .is_err(),
            "old bridge accepted export"
        );
    }
    assert!(rx.try_recv().is_err());
    connection.lock().unwrap().analysis_export = true;
    assert_eq!(handles.analysis_export_session(), Some(session));
    // Curated DNS consent is unrelated to summary support.
    assert!(!handles.advertises_curated_broad_opt_in(session));
    assert_eq!(handles.try_send_for_session(session, export(false)), Ok(()));
    assert_eq!(handles.try_send_for_session(session, export(true)), Ok(()));
    assert!(handles.try_send(export(false)).is_err());
    disconnect_and_discard(&connection, &mut rx);
    assert_eq!(handles.analysis_export_session(), None);
    let next = mark_connected(&connection, false);
    assert_eq!(
        handles.analysis_export_session(),
        None,
        "new bridge inherited capability"
    );
    connection.lock().unwrap().analysis_export = true;
    assert_eq!(
        handles.try_send_for_session(session, export(false)),
        Err(SendClientMessageError::StaleSession)
    );
    assert_eq!(handles.analysis_export_session(), Some(next));
}
