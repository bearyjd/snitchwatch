use super::*;
use snitchwatch_proto::protocol::ui_client::UiClient;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tonic::transport::Server;

// DaemonLiveness/StreamGuard's own unit tests now live in
// `daemon_liveness.rs`, next to the type they test.

/// Drains the channel: nothing but `PromptSlot` messages are left, and the
/// last says the slot is free (a trailing held one would be a stale prompt).
fn assert_only_a_free_slot_is_left(rx: &mut broadcast::Receiver<ServerMessage>) {
    let left: Vec<ServerMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        left.iter()
            .all(|m| matches!(m, ServerMessage::PromptSlot { .. })),
        "{left:?}"
    );
    assert!(
        matches!(
            left.last(),
            Some(ServerMessage::PromptSlot {
                holder: None,
                holders: 0,
                ..
            })
        ),
        "{left:?}"
    );
}

/// The next queued broadcast that isn't a `PromptSlot` (those are covered by
/// `prompt_slot_tests.rs`).
fn try_recv_skipping_slot(
    rx: &mut broadcast::Receiver<ServerMessage>,
) -> Result<ServerMessage, broadcast::error::TryRecvError> {
    loop {
        match rx.try_recv() {
            Ok(ServerMessage::PromptSlot { .. }) => continue,
            other => return other,
        }
    }
}

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

use crate::cache::connections::Verdict;
use crate::translator::connection::ask_row_id;
use crate::ws_messages::{VerdictDuration, VerdictScope};

#[tokio::test]
async fn persistent_allow_verdict_broadcasts_rule_for_live_clients() {
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
    );
    let _gui_session = svc.client_presence().authenticated_session();
    // A synced list: a remembered answer is announced only to one (H1).
    svc.rules_handle().lock().unwrap().replace_all(Vec::new());
    let svc = svc.into_server();
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
            VerdictDuration::Always,
            VerdictScope::ThisHost,
        )
        .unwrap();

    let rule = ask_handle.await.unwrap().unwrap().into_inner();
    assert_eq!(rule.action, "allow");
    assert_eq!(rule.duration, "always");
    assert!(!rule.name.is_empty());

    let update =
        try_recv_skipping_slot(&mut rx).expect("persistent verdict did not broadcast a rule");
    match update {
        ServerMessage::UpdateRules { rules } => {
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0]["name"], rule.name);
            assert_eq!(rules[0]["action"], "allow");
            assert_eq!(rules[0]["duration"], "always");
            // Issue #44: the remembered "This host" rule is bound to the
            // asking program, end to end through the wire shape.
            let operator = &rules[0]["operator"];
            assert_eq!(operator["type"], "list");
            assert_eq!(operator["operands"][0]["operand"], "process.path");
            assert_eq!(operator["operands"][0]["data"], "/usr/bin/curl");
            assert_eq!(operator["operands"][1]["operand"], "dest.host");
            assert_eq!(operator["operands"][1]["data"], "example.com");
            assert!(rule.name.contains("-pcurl-"), "got: {}", rule.name);
        }
        other => panic!("expected UpdateRules, got {other:?}"),
    }
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
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    let svc = svc.into_server();
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
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    let svc = svc.into_server();
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
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    let svc = svc.into_server();
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
        let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
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
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();

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
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();
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

#[tokio::test(start_paused = true)]
async fn recent_block_reverts_to_filter_off_while_paused() {
    // Issue #47: a prompt raised before the pause and denied mid-pause shows
    // RecentBlock, and its revert must land on FilterOff, not Idle.
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let filter_pause = Arc::new(FilterPause::new());
    let cache = Arc::new(Mutex::new(
        ConnectionCache::with_tray_publisher(64, tray_pub.clone())
            .with_filter_pause(filter_pause.clone()),
    ));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        Arc::new(crate::notice::NoticeBus::new()),
        filter_pause.clone(),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    let mut tray_rx = tray_pub.subscribe();

    let ask = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "tracker.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Pending(1));

    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    cache.lock().await.resync_tray_state();
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
    ask.await.unwrap().unwrap();
    assert!(matches!(
        &*tray_rx.borrow_and_update(),
        TrayState::RecentBlock { .. }
    ));

    tokio::time::advance(RECENT_BLOCK_TTL + Duration::from_millis(100)).await;
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::FilterOff);
}

#[tokio::test(start_paused = true)]
async fn a_deny_while_the_daemon_is_down_does_not_cover_the_daemon_down_tray() {
    // Issue #58 follow-up: a Deny that lands in a hung-daemon window used to
    // put "Blocked: X" over DaemonDown for the whole TTL.
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        64,
        tray_pub.clone(),
    )));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        Arc::new(crate::notice::NoticeBus::new()),
        Arc::new(FilterPause::new()),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    let mut tray_rx = tray_pub.subscribe();

    let ask = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "tracker.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });
    tray_rx.changed().await.unwrap();
    assert_eq!(*tray_rx.borrow(), TrayState::Pending(1));

    // The watchdog marks the daemon down while the prompt is open.
    {
        let mut cache = cache.lock().await;
        cache.set_daemon_down(true);
        tray_pub.set(cache.tray_state());
    }
    assert_eq!(*tray_rx.borrow_and_update(), TrayState::DaemonDown);

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
    ask.await.unwrap().unwrap();
    assert_eq!(
        *tray_rx.borrow_and_update(),
        TrayState::DaemonDown,
        "the block overlay must not cover DaemonDown"
    );

    // Nothing is left to revert, and recovery lands on the derived state.
    tokio::time::advance(RECENT_BLOCK_TTL + Duration::from_millis(100)).await;
    assert_eq!(*tray_rx.borrow(), TrayState::DaemonDown);
    {
        let mut cache = cache.lock().await;
        cache.set_daemon_down(false);
        tray_pub.set(cache.tray_state());
    }
    assert_eq!(*tray_rx.borrow(), TrayState::Idle);
}

#[test]
fn process_bound_verdict_rule_survives_the_wire_round_trip() {
    // Issue #44: toggling a rule in the GUI sends its wire shape back through
    // `rule_from_wire` as a CHANGE_RULE, which the daemon applies wholesale —
    // so the process binding must come back intact, case sensitivity included.
    let conn = Connection {
        protocol: "tcp".into(),
        dst_host: "example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        process_path: "/usr/bin/curl".into(),
        ..Default::default()
    };
    let rule = crate::translator::verdict::verdict_to_rule(
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::ThisHost,
        &conn,
        0,
    )
    .expect("absolute process path");
    let back = crate::rule_wire::rule_from_wire(&crate::rule_wire::rule_to_wire(&rule)).unwrap();
    assert_eq!(back.name, rule.name);
    let op = back.operator.unwrap();
    assert_eq!(op.r#type, "list");
    assert_eq!(op.list.len(), 2, "{op:?}");
    let (process, host) = (&op.list[0], &op.list[1]);
    assert_eq!(process.operand, "process.path");
    assert_eq!(process.data, "/usr/bin/curl");
    assert!(process.sensitive);
    assert_eq!(host.operand, "dest.host");
    assert_eq!(host.data, "example.com");
}

#[tokio::test]
async fn ask_rule_auto_allows_immediately_when_filtering_paused() {
    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus, filter_pause);
    // A pause only applies while a GUI is authenticated (see
    // `paused_bridge_without_an_authenticated_gui_defers_to_the_daemon`).
    let _gui_session = svc.client_presence().authenticated_session();

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
async fn paused_bridge_without_an_authenticated_gui_defers_to_the_daemon() {
    // Security review 2026-10-07 (issue #47): a pause is a GUI user's choice
    // and must not outlive every GUI session. With no authenticated GUI the
    // daemon's own default action applies, paused or not — otherwise a
    // paused system bridge would keep auto-allowing after logout.
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, mut rx) = broadcast::channel::<ServerMessage>(16);
    let filter_pause = Arc::new(FilterPause::new());
    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    let svc = UiService::new(
        cache.clone(),
        tx,
        Arc::new(crate::tray_state::TrayStatePublisher::new()),
        Arc::new(crate::notice::NoticeBus::new()),
        filter_pause,
    );

    let status = svc
        .ask_rule(Request::new(Connection {
            dst_host: "paused.example.com".into(),
            process_path: "/usr/bin/curl".into(),
            ..Default::default()
        }))
        .await
        .expect_err("no authenticated GUI: the daemon must decide");
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(
        cache.lock().await.len(),
        0,
        "nothing auto-allowed or recorded"
    );
    assert!(
        rx.try_recv().is_err(),
        "no decided row may be broadcast for a deferred ask"
    );
}

#[tokio::test]
async fn ask_rule_prompts_normally_when_not_paused() {
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let notice_bus = Arc::new(crate::notice::NoticeBus::new());
    let filter_pause = Arc::new(FilterPause::new());
    let svc = UiService::new(cache.clone(), tx, tray_pub, notice_bus, filter_pause);
    let _gui_session = svc.client_presence().authenticated_session();

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

#[tokio::test]
async fn a_departed_guis_pause_does_not_auto_allow_for_the_next_gui() {
    // Security review F1: the last-loss clear runs in its own task. A GUI that
    // authenticates before it runs must still be prompted, not auto-allowed
    // under the departed GUI's pause.
    let (svc, cache, mut rx) = lifecycle_service();
    let presence = svc.client_presence();
    let gui_a = presence.authenticated_session();
    crate::client_presence::apply_pause_request(
        &presence,
        &svc.filter_pause,
        crate::filter_pause::PauseRequest::Pause(Duration::from_secs(300)),
        Some(presence.current_generation()),
        None,
    );
    drop(gui_a); // No clear task in this test: the gap stays open.
    let _gui_b = presence.authenticated_session();
    assert!(svc.filter_pause.is_active_now());

    let ask = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "next-gui.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });
    let row_id = lifecycle_pending(&mut rx).await;
    assert_eq!(
        cache.lock().await.pending_count(),
        1,
        "GUI B was auto-allowed under GUI A's pause"
    );
    cache
        .lock()
        .await
        .resolve(
            &row_id,
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
        )
        .unwrap();
    assert_eq!(ask.await.unwrap().unwrap().into_inner().action, "deny");
}

#[tokio::test(start_paused = true)]
async fn an_expired_pause_prompts_before_its_expiry_tick_runs() {
    // Issue #47: `ask_rule` checks the deadline itself, so an expired pause
    // stops auto-allowing even before the expiry task clears it.
    use crate::translator::connection::ask_row_id;

    let tray_pub = Arc::new(crate::tray_state::TrayStatePublisher::new());
    let cache = Arc::new(Mutex::new(ConnectionCache::with_tray_publisher(
        64,
        tray_pub.clone(),
    )));
    let (tx, _rx) = broadcast::channel::<ServerMessage>(16);
    let filter_pause = Arc::new(FilterPause::new());
    let svc = UiService::new(
        cache.clone(),
        tx,
        tray_pub.clone(),
        Arc::new(crate::notice::NoticeBus::new()),
        filter_pause.clone(),
    );
    let _gui_session = svc.client_presence().authenticated_session();
    filter_pause.pause(Duration::from_secs(300), 0).unwrap();
    tokio::time::advance(Duration::from_secs(301)).await;
    let mut tray_rx = tray_pub.subscribe();

    let ask = tokio::spawn({
        let svc = svc.clone();
        async move {
            svc.ask_rule(Request::new(Connection {
                dst_host: "after-pause.example.com".into(),
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), tray_rx.changed())
        .await
        .expect("no prompt: the expired pause auto-allowed")
        .unwrap();
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
    assert_eq!(ask.await.unwrap().unwrap().into_inner().action, "deny");
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

// Lifecycle fixtures keep internal subscribers separate from GUI leases.
fn lifecycle_service() -> (
    UiService,
    Arc<Mutex<ConnectionCache>>,
    broadcast::Receiver<ServerMessage>,
) {
    let cache = Arc::new(Mutex::new(ConnectionCache::new(64)));
    let (tx, rx) = broadcast::channel(64);
    let svc = UiService::new(
        cache.clone(),
        tx,
        Arc::new(TrayStatePublisher::new()),
        Arc::new(NoticeBus::new()),
        Arc::new(FilterPause::new()),
    );
    (svc, cache, rx)
}

async fn lifecycle_pending(rx: &mut broadcast::Receiver<ServerMessage>) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let ServerMessage::InsertConnectionRows { rows } = rx.recv().await.unwrap() {
                return rows[0].id.clone();
            }
        }
    })
    .await
    .unwrap()
}

async fn lifecycle_removed(rx: &mut broadcast::Receiver<ServerMessage>, id: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match rx.recv().await.unwrap() {
                ServerMessage::RemoveConnectionRows { ids }
                    if ids.iter().any(|removed| removed == id) =>
                {
                    return
                }
                ServerMessage::UpdateRules { .. } | ServerMessage::UpdateConnectionRows { .. } => {
                    panic!("cancelled Ask produced a verdict side effect")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
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

// --- Issue #48: the daemon's rule list ---------------------------------------

use crate::cache::rules::RulesCache;
use crate::daemon_commands::CommandError;
use snitchwatch_proto::protocol::{NotificationReplyCode, Operator};

fn rules_service(
    transport: DaemonTransport,
) -> (
    UiService,
    Arc<Mutex<ConnectionCache>>,
    broadcast::Receiver<ServerMessage>,
) {
    let (svc, cache, rx) = lifecycle_service();
    (svc.with_daemon_transport(transport), cache, rx)
}

fn daemon_reply(id: u64, code: NotificationReplyCode, data: &str) -> NotificationReply {
    NotificationReply {
        id,
        code: code as i32,
        data: data.to_string(),
    }
}

fn hello() -> NotificationReply {
    daemon_reply(0, NotificationReplyCode::Ok, "")
}

fn daemon_rule(name: &str) -> Rule {
    Rule {
        name: name.to_string(),
        enabled: true,
        action: "allow".to_string(),
        duration: "always".to_string(),
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "dest.host".into(),
            data: "example.com".into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn with_rules(rules: Vec<Rule>) -> ClientConfig {
    ClientConfig {
        rules,
        ..Default::default()
    }
}

fn delete(name: &str) -> Notification {
    Notification {
        r#type: Action::DeleteRule as i32,
        rules: vec![Rule {
            name: name.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn set_rules_names(msg: ServerMessage) -> Vec<String> {
    match msg {
        ServerMessage::SetRules { rules } => rules
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect(),
        other => panic!("expected SetRules, got {other:?}"),
    }
}

fn cached(svc: &UiService) -> RulesCache {
    svc.rules_handle().lock().unwrap().clone()
}

#[tokio::test]
async fn subscribe_then_hello_commits_one_name_sorted_set_rules() {
    // Unix transport: a command needs a current stream, which makes the
    // "current is set before SetRules" guarantee observable.
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Unix);
    let synced = svc.rules.synced();
    let config = with_rules(vec![daemon_rule("c"), daemon_rule("a"), daemon_rule("b")]);

    // `Request::new` has no remote address: the shared `None` key.
    svc.subscribe(Request::new(config)).await.unwrap();
    assert_eq!(*synced.borrow(), 0, "Subscribe alone commits nothing");
    assert!(cached(&svc).is_unknown());
    assert!(rx.try_recv().is_err());

    let commands = svc.daemon_commands();
    let (stream, _outbound) = commands.open_stream(None);
    svc.daemon_commands().on_reply(stream.id(), &hello());

    assert_eq!(set_rules_names(rx.try_recv().unwrap()), vec!["a", "b", "c"]);
    assert!(
        commands.send(delete("a")).is_ok(),
        "a client that saw SetRules can send rule commands"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(ServerMessage::RulesNotShown { too_large: 0, .. })
    ));
    assert!(rx.try_recv().is_err(), "exactly one SetRules");
    assert_eq!(*synced.borrow(), 1);

    // The commit removed the staged snapshot.
    svc.daemon_commands().on_reply(stream.id(), &hello());
    assert!(rx.try_recv().is_err());
    assert_eq!(*synced.borrow(), 1);
}

/// Issue #61, the usual order at login: a GUI is already connected, there
/// is no list yet, and the daemon's snapshot is over the rule limit. The
/// GUI is told, without waiting for anything else to publish.
#[tokio::test]
async fn an_oversized_snapshot_with_no_list_is_reported_at_once() {
    use crate::cache::rules::MAX_SNAPSHOT_RULES;
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Unix);
    let config = with_rules(vec![daemon_rule("a"); MAX_SNAPSHOT_RULES + 1]);
    svc.subscribe(Request::new(config)).await.unwrap();
    let commands = svc.daemon_commands();
    let (stream, _outbound) = commands.open_stream(None);
    commands.on_reply(stream.id(), &hello());
    let sent: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let total = u32::try_from(MAX_SNAPSHOT_RULES + 1).unwrap();
    assert!(
        sent.iter().any(|m| matches!(
            m,
            ServerMessage::RulesNotShown { over_limit_total: Some(n), .. } if *n == total
        )),
        "{sent:?}"
    );
    assert!(cached(&svc).is_unknown());
}

/// PR #106 review M4: the bridge names a snapshot's `user.name` uids for
/// display, with the lookup it was given (a fake here: tests never read the
/// host's accounts), each uid once, canonical decimal only.
#[tokio::test]
async fn a_snapshot_carries_account_names_for_its_user_name_uids() {
    let looked_up = Arc::new(StdMutex::new(Vec::new()));
    let seen = looked_up.clone();
    let lookup: crate::accounts::AccountLookup = Arc::new(move |uid| {
        seen.lock().unwrap().push(uid);
        (uid == 958).then(|| "snitchwatch".to_string())
    });
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Unix);
    let svc = svc.with_account_lookup(lookup);
    let with_user = |name: &str, data: &str| Rule {
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "user.name".into(),
            data: data.into(),
            ..Default::default()
        }),
        ..daemon_rule(name)
    };
    let rules = vec![
        with_user("a", "958"),
        with_user("b", "0958"),
        with_user("c", "7"),
    ];
    svc.subscribe(Request::new(with_rules(rules.clone())))
        .await
        .unwrap();
    let commands = svc.daemon_commands();
    let (stream, _outbound) = commands.open_stream(None);
    commands.on_reply(stream.id(), &hello());
    let ServerMessage::SetRules { rules: wire } = rx.try_recv().unwrap() else {
        panic!("expected SetRules");
    };
    assert_eq!(
        wire[0]["userNames"],
        serde_json::json!({ "958": "snitchwatch" })
    );
    assert!(
        wire[1].get("userNames").is_none(),
        "0958 isn't a uid as written"
    );
    assert!(wire[2].get("userNames").is_none(), "no account 7");
    assert_eq!(*looked_up.lock().unwrap(), vec![7, 958]);
    // The next snapshot looks nothing up again.
    svc.subscribe(Request::new(with_rules(rules)))
        .await
        .unwrap();
    assert_eq!(looked_up.lock().unwrap().len(), 2);
}

fn over_limit_counts(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<Option<u32>> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|m| match m {
            ServerMessage::RulesNotShown {
                over_limit_total, ..
            } => Some(over_limit_total),
            _ => None,
        })
        .collect()
}

/// PR #106 review L1: an oversized snapshot's count is shown only for the
/// stream that sent it, once it says HELLO, and goes when it closes. On TCP
/// another connection's HELLO doesn't show it.
#[tokio::test]
async fn an_oversized_snapshot_is_counted_only_for_its_own_stream() {
    use crate::cache::rules::MAX_SNAPSHOT_RULES;
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Tcp);
    let (a, b) = (
        Some(std::net::SocketAddr::from(([127, 0, 0, 1], 1001))),
        Some(std::net::SocketAddr::from(([127, 0, 0, 1], 1002))),
    );
    svc.rules
        .stage(a, vec![daemon_rule("x"); MAX_SNAPSHOT_RULES + 1]);
    let commands = svc.daemon_commands();
    let (other, _other_rx) = commands.open_stream(b);
    commands.on_reply(other.id(), &hello());
    assert_eq!(over_limit_counts(&mut rx), Vec::<Option<u32>>::new());
    let (own, _own_rx) = commands.open_stream(a);
    commands.on_reply(own.id(), &hello());
    let total = u32::try_from(MAX_SNAPSHOT_RULES + 1).unwrap();
    assert_eq!(over_limit_counts(&mut rx), vec![Some(total)]);
    drop(own);
    assert_eq!(
        over_limit_counts(&mut rx),
        vec![None],
        "gone with its stream"
    );
}

/// PR #106 review OQ1, the Unix socket's one shared key: a redialled
/// daemon's old stream says HELLO late and adopts the new stream's
/// snapshot, then closes; the new stream's own HELLO adopts it again
/// instead of finding nothing, so the page isn't left empty.
#[tokio::test]
async fn a_late_hello_on_the_shared_key_does_not_leave_the_new_stream_without_a_list() {
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Unix);
    let commands = svc.daemon_commands();
    svc.subscribe(Request::new(with_rules(vec![daemon_rule("old")])))
        .await
        .unwrap();
    let (old, _old_rx) = commands.open_stream(None);
    svc.subscribe(Request::new(with_rules(vec![daemon_rule("new")])))
        .await
        .unwrap();
    let (new, _new_rx) = commands.open_stream(None);
    commands.on_reply(old.id(), &hello());
    assert_eq!(set_rules_names(rx.try_recv().unwrap()), vec!["new"]);
    drop(old);
    assert!(cached(&svc).is_unknown(), "withdrawn with the old stream");
    commands.on_reply(new.id(), &hello());
    assert_eq!(cached(&svc).snapshot_wire().unwrap().len(), 1);
    assert!(cached(&svc).contains("new"));
    // Its own HELLO again adopts nothing new.
    let synced = *svc.rules.synced().borrow();
    commands.on_reply(new.id(), &hello());
    assert_eq!(*svc.rules.synced().borrow(), synced);
}

#[tokio::test]
async fn hello_without_a_staged_snapshot_only_makes_its_stream_current() {
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Unix);
    svc.rules_handle()
        .lock()
        .unwrap()
        .replace_all(vec![daemon_rule("kept")]);
    let synced = svc.rules.synced();
    let commands = svc.daemon_commands();
    let (stream, _outbound) = commands.open_stream(None);

    svc.daemon_commands().on_reply(stream.id(), &hello());

    assert!(commands.send(delete("kept")).is_ok(), "stream is current");
    assert!(rx.try_recv().is_err());
    assert_eq!(*synced.borrow(), 0);
    assert_eq!(cached(&svc).snapshot_wire().unwrap().len(), 1);
}

#[tokio::test]
async fn a_subscribe_whose_connection_never_says_hello_never_replaces_the_cache() {
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Tcp);
    svc.subscribe(Request::new(with_rules(vec![daemon_rule("never")])))
        .await
        .unwrap();

    // Another connection's HELLO must not adopt it.
    let other = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 9)));
    let (stream, _outbound) = svc.daemon_commands().open_stream(other);
    svc.daemon_commands().on_reply(stream.id(), &hello());

    assert!(cached(&svc).is_unknown());
    assert!(rx.try_recv().is_err());
}

async fn ask_and_resolve(
    svc: &UiService,
    cache: &Arc<Mutex<ConnectionCache>>,
    rx: &mut broadcast::Receiver<ServerMessage>,
    duration: VerdictDuration,
) -> Rule {
    let ask_svc = svc.clone();
    let ask = tokio::spawn(async move {
        ask_svc
            .ask_rule(Request::new(Connection {
                protocol: "tcp".into(),
                dst_host: "example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            }))
            .await
    });
    let id = lifecycle_pending(rx).await;
    cache
        .lock()
        .await
        .resolve(&id, Verdict::Allow, duration, VerdictScope::ThisHost)
        .unwrap();
    ask.await.unwrap().unwrap().into_inner()
}

#[tokio::test]
async fn a_remembered_verdict_is_cached_and_a_one_shot_is_not() {
    let (svc, cache, mut rx) = rules_service(DaemonTransport::Tcp);
    let _gui = svc.client_presence().authenticated_session();
    svc.rules_handle().lock().unwrap().replace_all(Vec::new());

    let once = ask_and_resolve(&svc, &cache, &mut rx, VerdictDuration::Once).await;
    assert_eq!(cached(&svc).snapshot_wire(), Some(Vec::new()), "{once:?}");

    let always = ask_and_resolve(&svc, &cache, &mut rx, VerdictDuration::Always).await;
    let names: Vec<_> = cached(&svc)
        .snapshot_wire()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec![always.name]);
}

async fn next_set_rules(rx: &mut broadcast::Receiver<ServerMessage>) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let msg = rx.recv().await.unwrap();
            if matches!(msg, ServerMessage::SetRules { .. }) {
                return set_rules_names(msg);
            }
        }
    })
    .await
    .expect("no SetRules broadcast")
}

async fn next_command(stream: &mut Streaming<Notification>) -> Notification {
    tokio::time::timeout(Duration::from_secs(10), stream.message())
        .await
        .expect("no command reached the daemon stream")
        .unwrap()
        .unwrap()
}

/// Opens a daemon-side `Notifications` stream that says HELLO first, as
/// `listenForNotifications` does.
async fn open_daemon_stream(
    client: &mut UiClient<tonic::transport::Channel>,
) -> (
    tokio::sync::mpsc::Sender<NotificationReply>,
    Streaming<Notification>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tx.send(hello()).await.unwrap();
    let inbound = client
        .notifications(tokio_stream::wrappers::ReceiverStream::new(rx))
        .await
        .unwrap()
        .into_inner();
    (tx, inbound)
}

/// A redialing daemon subscribes on a new connection while its old stream
/// is still open: the new connection's snapshot is adopted on its own
/// HELLO, and only its replies are correlated. Two real tonic channels, so
/// the two connections have different remote addresses.
#[tokio::test]
async fn a_redial_adopts_the_new_connections_snapshot_and_correlates_its_replies() {
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Tcp);
    let commands = svc.daemon_commands();
    let mut ready = commands.stream_ready();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(svc.into_server())
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
    );
    let mut old = UiClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    let mut new = UiClient::connect(format!("http://{address}"))
        .await
        .unwrap();

    old.subscribe(with_rules(vec![daemon_rule("old-rule")]))
        .await
        .unwrap();
    let (old_replies, mut old_commands) = open_daemon_stream(&mut old).await;
    assert_eq!(next_set_rules(&mut rx).await, vec!["old-rule"]);

    new.subscribe(with_rules(vec![daemon_rule("new-b"), daemon_rule("new-a")]))
        .await
        .unwrap();
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|m| matches!(m, ServerMessage::SetRules { .. })),
        "staged until the new HELLO"
    );
    let (new_replies, mut new_commands) = open_daemon_stream(&mut new).await;
    assert_eq!(next_set_rules(&mut rx).await, vec!["new-a", "new-b"]);
    tokio::time::timeout(Duration::from_secs(10), ready.wait_for(|g| *g >= 2))
        .await
        .unwrap()
        .unwrap();

    // TCP keeps fan-out; the old connection's reply is ignored.
    let first = commands.send(delete("new-a")).unwrap();
    assert_eq!(next_command(&mut old_commands).await.id, first.id());
    assert_eq!(next_command(&mut new_commands).await.id, first.id());
    old_replies
        .send(daemon_reply(first.id(), NotificationReplyCode::Ok, ""))
        .await
        .unwrap();
    assert_eq!(
        first.wait(Duration::from_millis(300)).await,
        Err(CommandError::Timeout)
    );

    let second = commands.send(delete("new-a")).unwrap();
    assert_eq!(next_command(&mut new_commands).await.id, second.id());
    new_replies
        .send(daemon_reply(
            second.id(),
            NotificationReplyCode::Error,
            "nope",
        ))
        .await
        .unwrap();
    assert_eq!(
        second.wait(Duration::from_secs(10)).await,
        Err(CommandError::Rejected("nope".into()))
    );
    server.abort();
}
