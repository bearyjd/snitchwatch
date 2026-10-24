use super::*;

#[tokio::test]
async fn running_bridge_routes_export_privately_and_keeps_mock_ping_responsive() {
    let dir = tempfile::tempdir().unwrap();
    let bridge = crate::run(crate::BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 20,
    })
    .await
    .unwrap();
    let mut daemon =
        mock_opensnitchd::MockOpensnitchd::connect(bridge.grpc_endpoint.tcp_addr().unwrap())
            .await
            .unwrap();
    let mut broadcast = bridge.broadcast_tx.subscribe();
    let peer = Peer::new();
    bridge
        .inbound_tx
        .send(peer.request("real-service"))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), peer.session.next_reply()).await;
    let ping = daemon.ping(1).await;
    let messages: Vec<_> = std::iter::from_fn(|| broadcast.try_recv().ok()).collect();
    bridge.shutdown();
    assert!(
        result.is_ok(),
        "service did not publish the requested artifact"
    );
    assert!(result.unwrap().is_some());
    assert!(ping.is_ok());
    assert!(!messages.iter().any(|message| matches!(
        message,
        ServerMessage::CacheSummaryExport { .. } | ServerMessage::CacheSummaryExportRefused { .. }
    )));
}

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn source_selector_and_artifact_canaries_never_enter_application_logs() {
    use snitchwatch_bridge::{analysis_export::AppSelector, ws_messages::ConnectionRow};
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("off,snitchwatch_bridge=trace,snitchwatch_bridge_cli=trace")
        .with_ansi(false)
        .with_writer(move || Capture(sink.clone()))
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let _guard = tracing::dispatcher::set_default(&dispatch);
    tracing::info!(target: "snitchwatch_bridge_cli", "static-export-test-marker");
    let cache = cache();
    cache.lock().await.insert_decided(ConnectionRow {
        id: "row-canary".into(),
        process: "process-canary".into(),
        process_path: Some("/home/selector-canary".into()),
        dst_host: "host-canary.invalid".into(),
        dst_ip: "203.0.113.7".into(),
        dst_port: 443,
        protocol: "tcp".into(),
        direction: "out".into(),
        action: Some("allow".into()),
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 1,
        matched_rule: Some("rule-canary".into()),
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    });
    let service = AnalysisExports::new(cache);
    let peer = Peer::new();
    let mut references = Vec::new();
    for id in ["privacy-1", "privacy-2"] {
        let mut request = peer.request(id);
        if let ClientMessage::RequestCacheSummaryExport { filters, .. } = &mut request {
            filters.app = Some(AppSelector::Path {
                value: "/home/selector-canary".into(),
            });
        }
        service.try_route(request);
        finish(&service).await;
        let pending = tokio::time::timeout(Duration::from_secs(1), peer.session.next_reply())
            .await
            .unwrap()
            .unwrap();
        for canary in [
            "row-canary",
            "process-canary",
            "/home/selector-canary",
            "host-canary.invalid",
            "203.0.113.7",
            "rule-canary",
        ] {
            assert!(!pending.wire.contains(canary));
        }
        let wire: serde_json::Value = serde_json::from_str(&pending.wire).unwrap();
        let document: serde_json::Value =
            serde_json::from_str(wire["artifact"].as_str().unwrap()).unwrap();
        assert!(document["groups"][0]["appRef"].is_string());
        references.push(document["groups"][0]["appRef"].clone());
    }
    assert_ne!(
        references[0], references[1],
        "fresh exports reused an identity key"
    );
    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("static-export-test-marker"));
    for canary in [
        "row-canary",
        "process-canary",
        "selector-canary",
        "host-canary",
        "203.0.113.7",
        "rule-canary",
    ] {
        assert!(!logs.contains(canary));
    }
}
