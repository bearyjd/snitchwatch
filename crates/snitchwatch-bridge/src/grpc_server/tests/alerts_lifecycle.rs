use super::*;

fn text_alert(
    what: snitchwatch_proto::protocol::alert::What,
    r#type: snitchwatch_proto::protocol::alert::Type,
    text: &str,
) -> Alert {
    Alert {
        id: 1,
        r#type: r#type as i32,
        action: 0,
        priority: 0,
        what: what as i32,
        data: Some(snitchwatch_proto::protocol::alert::Data::Text(
            text.to_string(),
        )),
    }
}

#[tokio::test]
async fn post_alert_records_error_into_alert_store() {
    use snitchwatch_proto::protocol::alert;

    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filter_pause);

    let alert = text_alert(
        alert::What::ProcMonitor,
        alert::Type::Error,
        "eBPF module failed to load",
    );
    svc.post_alert(Request::new(alert)).await.unwrap();

    let stored = svc.alert_store_handle().get(alert::What::ProcMonitor);
    assert_eq!(
        stored.map(|s| s.text),
        Some("eBPF module failed to load".to_string())
    );
}

#[tokio::test]
async fn post_alert_with_wired_diagnostics_ctx_broadcasts_fresh_report() {
    use crate::diagnostics::kernel_probe::testing::FakeKernelProbe;
    use crate::diagnostics::DiagnosticsCtx;
    use snitchwatch_proto::protocol::alert;

    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filter_pause);

    let probe: Arc<dyn crate::diagnostics::kernel_probe::KernelProbe> =
        Arc::new(FakeKernelProbe::all_ok());
    let ctx = Arc::new(DiagnosticsCtx::new(
        svc.liveness_handle(),
        svc.firewall_status_handle(),
        probe,
        svc.alert_store_handle(),
    ));
    svc.set_diagnostics_ctx(ctx);

    let alert = text_alert(
        alert::What::Firewall,
        alert::Type::Error,
        "nftables backend unavailable",
    );
    svc.post_alert(Request::new(alert)).await.unwrap();

    let broadcasted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("post_alert did not broadcast a diagnostics report")
        .expect("broadcast error");
    assert!(matches!(
        broadcasted,
        ServerMessage::DiagnosticsReport { .. }
    ));
}

#[tokio::test]
async fn post_alert_without_wired_diagnostics_ctx_does_not_broadcast() {
    use snitchwatch_proto::protocol::alert;

    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filter_pause);

    let alert = text_alert(alert::What::Firewall, alert::Type::Error, "nft down");
    svc.post_alert(Request::new(alert)).await.unwrap();

    // No DiagnosticsCtx wired up: recording happens, but nothing is
    // broadcast to a receiver that would otherwise hang waiting for it.
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn subscribe_does_not_clear_previously_stored_alerts() {
    // A fresh `subscribe()` (e.g. a plain reconnect, not a fix) must not
    // erase a still-true alert — see `daemon_alerts`'s module doc for
    // why this changed from an earlier clear-on-subscribe design.
    // Clearing is now `ClientMessage::RecheckDiagnostics`'s job, tested
    // at the `DiagnosticsCtx::clear_alerts` level in `diagnostics/mod.rs`.
    use snitchwatch_proto::protocol::alert;

    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filter_pause);

    let alert = text_alert(alert::What::ProcMonitor, alert::Type::Error, "boom");
    svc.post_alert(Request::new(alert)).await.unwrap();
    assert!(svc
        .alert_store_handle()
        .get(alert::What::ProcMonitor)
        .is_some());

    svc.subscribe(Request::new(ClientConfig::default()))
        .await
        .unwrap();

    assert!(svc
        .alert_store_handle()
        .get(alert::What::ProcMonitor)
        .is_some());
}

#[test]
fn display_summary_sanitizes_hostile_process_and_host_text() {
    let s = display_summary(
        "evil\u{1b}[2Jname\u{202e}",
        "<script>bad</script>.example.com",
    );
    assert!(!s.contains('\u{1b}'), "ANSI escape must be stripped: {s:?}");
    assert!(
        !s.contains('\u{202e}'),
        "bidi override must be stripped: {s:?}"
    );
    assert!(
        !s.contains('<') && !s.contains('>'),
        "markup escaped: {s:?}"
    );
    assert!(s.contains(" → "), "keeps the summary shape: {s:?}");
}

#[tokio::test]
async fn no_gui_returns_unavailable_even_with_internal_broadcast_receiver() {
    let (svc, cache, mut rx) = lifecycle_service();
    let status = svc
        .ask_rule(Request::new(Connection::default()))
        .await
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert!(cache.lock().await.is_empty());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn admission_loss_before_insertion_is_latched_across_reconnect() {
    let (svc, cache, mut rx) = lifecycle_service();
    let lease = svc.client_presence().authenticated_session();
    let cache_lock = cache.lock().await;
    let mut ask = Box::pin(svc.ask_rule(Request::new(Connection::default())));
    assert!(futures_util::poll!(&mut ask).is_pending());
    drop(lease);
    let _reconnected = svc.client_presence().authenticated_session();
    drop(cache_lock);
    let status = ask.await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert!(cache.lock().await.is_empty());
    assert!(
        rx.try_recv().is_err(),
        "lost admission must not publish a prompt"
    );
}

#[tokio::test]
async fn last_disconnect_cancels_old_ask_despite_reconnect_and_late_verdict() {
    let (svc, cache, mut rx) = lifecycle_service();
    let lease = svc.client_presence().authenticated_session();
    let ask_svc = svc.clone();
    let ask =
        tokio::spawn(async move { ask_svc.ask_rule(Request::new(Connection::default())).await });
    let id = lifecycle_pending(&mut rx).await;
    drop(lease);
    let _new_client = svc.client_presence().authenticated_session();
    let status = tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unavailable);
    lifecycle_removed(&mut rx, &id).await;
    let mut cache = cache.lock().await;
    assert_eq!(cache.pending_count(), 0);
    assert!(cache.is_empty());
    assert!(cache
        .resolve(
            &id,
            Verdict::Allow,
            VerdictDuration::Always,
            crate::ws_messages::VerdictScope::AnyHost
        )
        .is_err());
    assert!(cache.is_empty());
    assert_only_a_free_slot_is_left(&mut rx);
}

#[tokio::test]
async fn rpc_future_drop_under_cache_contention_rejects_verdict_and_cleans_row() {
    let (svc, cache, mut rx) = lifecycle_service();
    let _lease = svc.client_presence().authenticated_session();
    let ask = tokio::spawn(async move { svc.ask_rule(Request::new(Connection::default())).await });
    let id = lifecycle_pending(&mut rx).await;
    let mut locked = cache.lock().await;
    ask.abort();
    assert!(ask.await.unwrap_err().is_cancelled());
    // The receiver has dropped, while the cleanup task is blocked by us.
    assert!(locked
        .resolve(
            &id,
            Verdict::Allow,
            VerdictDuration::Always,
            crate::ws_messages::VerdictScope::ThisHost
        )
        .is_err());
    assert!(locked.is_empty());
    drop(locked);
    lifecycle_removed(&mut rx, &id).await;
    tokio::task::yield_now().await;
    assert_eq!(cache.lock().await.pending_count(), 0);
    assert!(rx.try_recv().is_err(), "cleanup must not duplicate removal");
}

#[tokio::test]
async fn tonic_client_abort_removes_real_server_pending_request() {
    let (svc, cache, mut rx) = lifecycle_service();
    let _lease = svc.client_presence().authenticated_session();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(svc.into_server())
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );
    let mut client = UiClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let mut ask_client = client.clone();
    let ask = tokio::spawn(async move { ask_client.ask_rule(Connection::default()).await });
    let id = lifecycle_pending(&mut rx).await;
    ask.abort();
    assert!(ask.await.unwrap_err().is_cancelled());
    lifecycle_removed(&mut rx, &id).await;
    assert!(cache.lock().await.is_empty());
    assert_eq!(cache.lock().await.pending_count(), 0);
    assert_eq!(
        client
            .ping(PingRequest {
                id: 17,
                stats: None
            })
            .await
            .unwrap()
            .into_inner()
            .id,
        17
    );
    server.abort();
}

#[tokio::test]
async fn verdict_that_wins_before_last_disconnect_is_preserved() {
    let (svc, cache, mut rx) = lifecycle_service();
    let lease = svc.client_presence().authenticated_session();
    let ask_svc = svc.clone();
    let ask =
        tokio::spawn(async move { ask_svc.ask_rule(Request::new(Connection::default())).await });
    let id = lifecycle_pending(&mut rx).await;
    cache
        .lock()
        .await
        .resolve(
            &id,
            Verdict::Allow,
            VerdictDuration::Once,
            crate::ws_messages::VerdictScope::ThisHost,
        )
        .unwrap();
    drop(lease);
    let rule = tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_inner();
    assert_eq!(rule.action, "allow");
    let cache = cache.lock().await;
    assert_eq!(cache.pending_count(), 0);
    assert_eq!(cache.rows()[0].action.as_deref(), Some("allow"));
    assert_only_a_free_slot_is_left(&mut rx);
}

#[tokio::test]
async fn tonic_request_deadline_cleans_pending_with_silent_authenticated_gui() {
    let (svc, cache, mut rx) = lifecycle_service();
    let _lease = svc.client_presence().authenticated_session();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(svc.into_server())
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );
    let mut client = UiClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let mut timed_client = client.clone();
    let ask = tokio::spawn(async move {
        let mut request = Request::new(Connection::default());
        request.set_timeout(Duration::from_millis(100));
        timed_client.ask_rule(request).await
    });
    let id = lifecycle_pending(&mut rx).await;
    let error = tokio::time::timeout(Duration::from_secs(2), ask)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        error.code(),
        tonic::Code::Cancelled | tonic::Code::DeadlineExceeded
    ));
    lifecycle_removed(&mut rx, &id).await;
    assert!(cache.lock().await.is_empty());
    assert_eq!(
        client
            .ping(PingRequest {
                id: 18,
                stats: None
            })
            .await
            .unwrap()
            .into_inner()
            .id,
        18
    );
    server.abort();
}
