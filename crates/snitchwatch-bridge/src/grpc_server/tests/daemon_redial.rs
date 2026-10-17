use super::*;

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

/// Subscribe received these rules before account lookup blocked. A refusal
/// during that await is newer than the received list, even if staging is later.
#[tokio::test(start_paused = true)]
async fn a_subscribe_blocked_in_account_lookup_keeps_later_refusal_markers() {
    use crate::cache::rules::MAX_RULE_FIELD_BYTES;

    let (entered, entry) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = StdMutex::new(released);
    let lookup: crate::accounts::AccountLookup = Arc::new(move |uid| {
        assert_eq!(uid, 958);
        entered.send(()).unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .expect("the test must release account lookup");
        Some("snitchwatch".to_string())
    });
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Unix);
    let svc = svc.with_account_lookup(lookup);
    let with_user = |name: &str| Rule {
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "user.name".into(),
            data: "958".into(),
            ..Default::default()
        }),
        ..daemon_rule(name)
    };
    let listed = with_user("listed");
    let mut oversized = with_user("oversized");
    oversized.description = "x".repeat(MAX_RULE_FIELD_BYTES + 1);
    let config = with_rules(vec![listed, oversized]);
    let subscribed = svc.subscribe(Request::new(config.clone()));
    tokio::pin!(subscribed);
    assert!(futures_util::poll!(&mut subscribed).is_pending());
    // The paused clock prevents the production lookup timeout from racing
    // this bounded handshake with the blocking worker.
    entry
        .recv_timeout(Duration::from_secs(5))
        .expect("Subscribe must have entered account lookup");
    svc.rules.apply_refused(&delete("listed"));
    svc.rules.apply_refused(&delete("oversized"));
    release.send(()).unwrap();
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(5), &mut subscribed)
        .await
        .expect("Subscribe must finish after lookup is released")
        .unwrap();

    let commands = svc.daemon_commands();
    let (stream, _outbound) = commands.open_stream(None);
    commands.on_reply(stream.id(), &hello());
    let cache = cached(&svc);
    assert!(cache.contains("listed"), "the received list was adopted");
    assert!(cache.left_out().contains_key("oversized"));
    assert_eq!(
        cache.snapshot_wire().unwrap()[0]["userNames"],
        serde_json::json!({ "958": "snitchwatch" }),
        "the lookup completed before adoption"
    );
    for name in ["listed", "oversized"] {
        assert!(
            cache.files_left().contains(name),
            "a snapshot received before blocked lookup cannot clear a later refusal: {name}"
        );
    }
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok()).any(|message| matches!(
            message,
            ServerMessage::RulesNotShown {
                left_on_disk: 2,
                ..
            }
        ))
    );

    // A genuinely newer Subscribe still proves both files were loaded.
    svc.subscribe(Request::new(config)).await.unwrap();
    let (newer, _newer_outbound) = commands.open_stream(None);
    commands.on_reply(newer.id(), &hello());
    let fresh = cached(&svc);
    for name in ["listed", "oversized"] {
        assert!(
            !fresh.files_left().contains(name),
            "a snapshot received after the refusal must clear its listed marker: {name}"
        );
    }
}
