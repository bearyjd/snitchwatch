use super::*;
use crate::translator::connection::ask_row_id;

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
