use super::*;
use snitchwatch_proto::protocol::ui_client::UiClient;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tonic::transport::Server;

// DaemonLiveness/StreamGuard's own unit tests now live in
// `daemon_liveness.rs`, next to the type they test.

async fn spawn_test_service() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel(16);
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache,
        tx,
        tray_pub,
        notice_bus,
        Arc::new(AtomicBool::new(false)),
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
    addr
}

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
        Arc::new(AtomicBool::new(false)),
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
        Arc::new(AtomicBool::new(false)),
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
    let filtering_paused = Arc::new(AtomicBool::new(false));
    let service = UiService::new(cache, tx, tray_pub, notice_bus, filtering_paused);

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

use crate::cache::connections::Verdict;
use crate::translator::connection::ask_row_id;
use crate::ws_messages::{VerdictDuration, VerdictScope};

#[tokio::test]
async fn ask_rule_blocks_until_cache_resolves_with_allow() {
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
        Arc::new(AtomicBool::new(false)),
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

    let ask_handle = tokio::spawn(async move {
        client
            .ask_rule(Connection {
                protocol: "tcp".into(),
                dst_host: "example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            })
            .await
    });

    let inserted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("ask_rule did not broadcast")
        .expect("broadcast error");
    let row_id = match inserted {
        ServerMessage::InsertConnectionRows { rows } => rows[0].id.clone(),
        other => panic!("expected InsertConnectionRows, got {other:?}"),
    };
    assert_eq!(row_id, ask_row_id(1));

    cache
        .lock()
        .await
        .resolve(
            &row_id,
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();

    let rule = ask_handle.await.unwrap().unwrap().into_inner();
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "once");
    assert!(!rule.name.is_empty());
}

#[tokio::test]
async fn ask_rule_returns_deny_rule_when_resolved_with_deny() {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub,
        notice_bus,
        Arc::new(AtomicBool::new(false)),
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

    let ask_handle = tokio::spawn(async move {
        client
            .ask_rule(Connection {
                protocol: "tcp".into(),
                dst_host: "tracker.example.com".into(),
                dst_ip: "1.2.3.4".into(),
                dst_port: 80,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            })
            .await
    });

    let row_id = ask_row_id(1);
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if cache
            .lock()
            .await
            .resolve(
                &row_id,
                Verdict::Deny,
                VerdictDuration::Once,
                VerdictScope::ThisHost,
            )
            .is_ok()
        {
            break;
        }
    }

    let rule = ask_handle.await.unwrap().unwrap().into_inner();
    assert_eq!(rule.action, "deny");
}

#[tokio::test]
async fn deny_scope_narrowed_notice_sanitizes_attacker_chosen_process_name() {
    // Issue #14 security review round 2 follow-up: `row.process` (the
    // basename of `process_path`) is daemon-attested *existence* only — a
    // local user still fully controls the path/basename text itself (e.g.
    // executing `/tmp/<b>evil</b>\x1b[31m`). It must be sanitized the same
    // way `dst_host` is before reaching the `DenyScopeNarrowed` notice body.
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let mut notice_rx = notice_bus.subscribe();
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub,
        notice_bus,
        Arc::new(AtomicBool::new(false)),
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

    let ask_handle = tokio::spawn(async move {
        client
            .ask_rule(Connection {
                protocol: "tcp".into(),
                // "shop.co.uk" degrades AnyHostOnDomain (co.uk is a
                // 2-label eTLD) — guarantees a DenyScopeNarrowed notice.
                dst_host: "shop.co.uk".into(),
                dst_ip: "1.2.3.4".into(),
                dst_port: 443,
                process_path: "/tmp/<b>evil</b>\x1b[31m".into(),
                ..Default::default()
            })
            .await
    });

    let row_id = ask_row_id(1);
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if cache
            .lock()
            .await
            .resolve(
                &row_id,
                Verdict::Deny,
                VerdictDuration::Once,
                VerdictScope::AnyHostOnDomain,
            )
            .is_ok()
        {
            break;
        }
    }
    let _ = ask_handle.await.unwrap().unwrap();

    // The bus also carries the earlier `Pending` notice for this same ask —
    // skip past it to find the `DenyScopeNarrowed` one.
    let what = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match notice_rx.recv().await.expect("notice_bus closed") {
                crate::notice::Notice::DenyScopeNarrowed { what, .. } => return what,
                _ => continue,
            }
        }
    })
    .await
    .expect("timed out waiting for DenyScopeNarrowed notice");

    assert!(!what.contains('<'), "markup must not survive: {what:?}");
    assert!(!what.contains('>'), "markup must not survive: {what:?}");
    assert!(
        !what.contains('\x1b'),
        "ANSI escape must not survive: {what:?}"
    );
}

#[tokio::test]
async fn two_concurrent_ask_rules_get_distinct_ask_ids() {
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
        Arc::new(AtomicBool::new(false)),
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

    for _ in 0..2 {
        let endpoint = format!("http://{addr}");
        tokio::spawn(async move {
            let channel = tonic::transport::Endpoint::from_shared(endpoint)
                .unwrap()
                .connect()
                .await
                .unwrap();
            let mut client = UiClient::new(channel);
            let _ = client.ask_rule(Connection::default()).await;
        });
    }

    let mut seen = std::collections::HashSet::new();
    while seen.len() < 2 {
        let msg = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("missed broadcast")
            .expect("broadcast error");
        if let ServerMessage::InsertConnectionRows { rows } = msg {
            for r in rows {
                seen.insert(r.id);
            }
        }
    }
    assert!(seen.contains(&ask_row_id(1)));
    assert!(seen.contains(&ask_row_id(2)));

    let _ = cache.lock().await.resolve(
        &ask_row_id(1),
        Verdict::Deny,
        VerdictDuration::Once,
        VerdictScope::ThisHost,
    );
    let _ = cache.lock().await.resolve(
        &ask_row_id(2),
        Verdict::Deny,
        VerdictDuration::Once,
        VerdictScope::ThisHost,
    );
}

#[tokio::test(start_paused = true)]
async fn ask_rule_deny_publishes_recent_block_then_reverts_to_idle() {
    use crate::translator::connection::ask_row_id;
    use crate::ws_messages::{VerdictDuration, VerdictScope};

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        64,
        tray_pub.clone(),
    )));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        notice_bus,
        Arc::new(AtomicBool::new(false)),
    );

    let mut tray_rx = tray_pub.subscribe();

    let svc_for_ask = svc.clone();
    let ask_handle = tokio::spawn(async move {
        svc_for_ask
            .ask_rule(Request::new(Connection {
                protocol: "tcp".into(),
                dst_host: "tracker.example.com".into(),
                dst_ip: "1.2.3.4".into(),
                dst_port: 80,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
    });

    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Pending(1));

    cache
        .lock()
        .await
        .resolve(
            &ask_row_id(1),
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    ask_handle.await.unwrap().unwrap();

    tray_rx.changed().await.unwrap();
    match &*tray_rx.borrow() {
        TrayState::RecentBlock { what, .. } => {
            assert!(what.contains("tracker.example.com"), "unexpected: {what}")
        }
        other => panic!("expected RecentBlock, got {other:?}"),
    }

    tokio::time::advance(RECENT_BLOCK_TTL + Duration::from_millis(100)).await;
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Idle);
}

#[tokio::test(start_paused = true)]
async fn second_deny_within_ttl_supersedes_first_blocks_revert_timer() {
    use crate::translator::connection::ask_row_id;
    use crate::ws_messages::{VerdictDuration, VerdictScope};

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        64,
        tray_pub.clone(),
    )));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        notice_bus,
        Arc::new(AtomicBool::new(false)),
    );
    let mut tray_rx = tray_pub.subscribe();

    // First block.
    let svc1 = svc.clone();
    let ask1 = tokio::spawn(async move {
        svc1.ask_rule(Request::new(Connection {
            dst_host: "first.example.com".into(),
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }))
        .await
    });
    tray_rx.changed().await.unwrap();
    cache
        .lock()
        .await
        .resolve(
            &ask_row_id(1),
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    ask1.await.unwrap().unwrap();
    tray_rx.changed().await.unwrap();
    assert!(matches!(&*tray_rx.borrow(), TrayState::RecentBlock { .. }));

    // Halfway through the first block's TTL, a second block supersedes it.
    tokio::time::advance(RECENT_BLOCK_TTL / 2).await;
    let svc2 = svc.clone();
    let ask2 = tokio::spawn(async move {
        svc2.ask_rule(Request::new(Connection {
            dst_host: "second.example.com".into(),
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }))
        .await
    });
    tray_rx.changed().await.unwrap(); // Pending(1) for the second ask
    cache
        .lock()
        .await
        .resolve(
            &ask_row_id(2),
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    ask2.await.unwrap().unwrap();
    tray_rx.changed().await.unwrap();
    match &*tray_rx.borrow() {
        TrayState::RecentBlock { what, .. } => assert!(what.contains("second.example.com")),
        other => panic!("expected RecentBlock(second), got {other:?}"),
    }

    // When the FIRST block's original TTL would have elapsed, its timer
    // must be a no-op — the tray should still show the second block.
    tokio::time::advance(RECENT_BLOCK_TTL / 2 + Duration::from_millis(50)).await;
    assert!(
        matches!(&*tray_rx.borrow(), TrayState::RecentBlock { what, .. } if what.contains("second.example.com")),
        "first block's timer must not have reverted the tray"
    );

    // Only once the SECOND block's own TTL elapses does it revert.
    tokio::time::advance(RECENT_BLOCK_TTL).await;
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Idle);
}

#[tokio::test]
async fn ask_rule_auto_allows_immediately_when_filtering_paused() {
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filtering_paused = Arc::new(AtomicBool::new(true));
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub,
        notice_bus,
        filtering_paused.clone(),
    );

    // No spawn/wait needed: paused ask_rule never blocks on a oneshot.
    let rule = svc
        .ask_rule(Request::new(Connection {
            dst_host: "paused.example.com".into(),
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(rule.action, "allow");

    // No pending row was ever created.
    assert_eq!(cache.lock().await.pending_count(), 0);
    assert_eq!(cache.lock().await.len(), 1);

    let broadcasted = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("paused ask_rule did not broadcast the decided row")
        .expect("broadcast error");
    match broadcasted {
        ServerMessage::InsertConnectionRows { rows } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].action.as_deref(), Some("allow"));
        }
        other => panic!("expected InsertConnectionRows, got {other:?}"),
    }
}

#[tokio::test]
async fn ask_rule_prompts_normally_when_not_paused() {
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filtering_paused = Arc::new(AtomicBool::new(false));
    let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus, filtering_paused);

    let ask_handle = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "normal.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });

    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if cache
            .lock()
            .await
            .resolve(
                &ask_row_id(1),
                Verdict::Allow,
                VerdictDuration::Once,
                VerdictScope::ThisHost,
            )
            .is_ok()
        {
            break;
        }
    }

    let rule = ask_handle.await.unwrap().unwrap().into_inner();
    assert_eq!(rule.action, "allow");
}

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
    let filtering_paused = Arc::new(AtomicBool::new(false));
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filtering_paused);

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
    let filtering_paused = Arc::new(AtomicBool::new(false));
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filtering_paused);

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
    let filtering_paused = Arc::new(AtomicBool::new(false));
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filtering_paused);

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
    let filtering_paused = Arc::new(AtomicBool::new(false));
    let svc = UiService::new(cache, tx, tray_pub, notice_bus, filtering_paused);

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
