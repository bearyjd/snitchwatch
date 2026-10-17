use super::*;

#[tokio::test]
async fn ping_round_trips_id() {
    let addr = spawn_test_service().await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = UiClient::new(channel);
    let reply = client
        .ping(PingRequest {
            id: 99,
            stats: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(reply.id, 99);
}

#[tokio::test]
async fn ping_with_stats_events_inserts_decided_rows_with_matched_rule() {
    use snitchwatch_proto::protocol::{Event, Rule as ProtoRule, Statistics};

    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub,
        notice_bus,
        Arc::new(FilterPause::new()),
    )
    .into_server();
    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = UiClient::new(channel);

    let event = Event {
        time: "2026-07-05T00:00:00Z".to_string(),
        connection: Some(Connection {
            protocol: "tcp".into(),
            dst_host: "example.com".into(),
            dst_ip: "93.184.216.34".into(),
            dst_port: 443,
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }),
        rule: Some(ProtoRule {
            created: 1_700_000_000,
            name: "899-curl-allow-out.json".into(),
            description: String::new(),
            enabled: true,
            precedence: false,
            nolog: false,
            action: "allow".into(),
            duration: "always".into(),
            operator: None,
        }),
        unixnano: 1_700_000_000_000_000_000,
    };

    let reply = client
        .ping(PingRequest {
            id: 7,
            stats: Some(Statistics {
                events: vec![event],
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(reply.id, 7);

    let broadcasted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("ping did not broadcast the decided row")
        .expect("broadcast error");
    match broadcasted {
        ServerMessage::InsertConnectionRows { rows } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].dst_host, "example.com");
            assert_eq!(rows[0].action.as_deref(), Some("allow"));
            assert_eq!(
                rows[0].matched_rule.as_deref(),
                Some("899-curl-allow-out.json")
            );
        }
        other => panic!("expected InsertConnectionRows, got {other:?}"),
    }
    assert_eq!(cache.lock().await.len(), 1);
}

#[tokio::test]
async fn ping_with_stats_broadcasts_daemon_statistics() {
    use snitchwatch_proto::protocol::Statistics;

    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache,
        tx,
        tray_pub,
        notice_bus,
        Arc::new(FilterPause::new()),
    )
    .into_server();
    tokio::spawn(async move {
        Server::builder()
            .add_service(svc)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = UiClient::new(channel);

    // No `events`, only aggregate scalars — the broadcast must not be gated
    // on `events` being non-empty.
    let reply = client
        .ping(PingRequest {
            id: 11,
            stats: Some(Statistics {
                daemon_version: "1.8.0".into(),
                rules: 12,
                uptime: 3661,
                connections: 4200,
                ignored: 10,
                accepted: 4000,
                dropped: 200,
                rule_hits: 3900,
                rule_misses: 300,
                events: vec![],
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(reply.id, 11);

    let broadcasted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("ping did not broadcast daemon statistics")
        .expect("broadcast error");
    match broadcasted {
        ServerMessage::DaemonStatistics {
            daemon_version,
            uptime,
            rules,
            connections,
            ignored,
            accepted,
            dropped,
            rule_hits,
            rule_misses,
        } => {
            assert_eq!(daemon_version, "1.8.0");
            assert_eq!(uptime, 3661);
            assert_eq!(rules, 12);
            assert_eq!(connections, 4200);
            assert_eq!(ignored, 10);
            assert_eq!(accepted, 4000);
            assert_eq!(dropped, 200);
            assert_eq!(rule_hits, 3900);
            assert_eq!(rule_misses, 300);
        }
        other => panic!("expected DaemonStatistics, got {other:?}"),
    }
}

#[tokio::test]
async fn ping_without_stats_is_a_noop() {
    let addr = spawn_test_service().await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = UiClient::new(channel);
    let reply = client
        .ping(PingRequest { id: 3, stats: None })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(reply.id, 3);
}

#[tokio::test]
async fn subscribe_captures_firewall_status() {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    let service = UiService::new(cache, tx, tray_pub, notice_bus, filter_pause);

    let handle = service.firewall_status_handle();
    assert_eq!(*handle.lock().unwrap(), None);

    let cfg = ClientConfig {
        is_firewall_running: true,
        ..Default::default()
    };
    let _ = service.subscribe(Request::new(cfg)).await.unwrap();

    assert_eq!(*handle.lock().unwrap(), Some(true));
}

#[tokio::test]
async fn subscribe_echoes_config() {
    let addr = spawn_test_service().await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = UiClient::new(channel);
    let cfg = ClientConfig {
        id: 1,
        name: "opensnitchd-test".to_string(),
        version: "1.6.0".to_string(),
        ..Default::default()
    };
    let echoed = client.subscribe(cfg.clone()).await.unwrap().into_inner();
    assert_eq!(echoed.name, cfg.name);
    assert_eq!(echoed.version, cfg.version);
}
