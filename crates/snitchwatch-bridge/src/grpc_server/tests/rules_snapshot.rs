use super::*;

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

/// The daemon gives `Subscribe` 10 s (`notifications.go`) and redials if
/// it takes longer, so slow account lookups (NSS over the network) must not
/// hold it: the reply waits a bounded time, the snapshot is staged and
/// committed regardless, and names that come late are cached and published
/// then, and never looked up again.
#[tokio::test]
async fn slow_account_lookups_do_not_hold_up_subscribe() {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = StdMutex::new(released);
    let calls = Arc::new(StdMutex::new(0));
    let counted = calls.clone();
    let lookup: crate::accounts::AccountLookup = Arc::new(move |_| {
        *counted.lock().unwrap() += 1;
        let _ = released.lock().unwrap().recv();
        Some("snitchwatch".to_string())
    });
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Unix);
    let svc = svc.with_account_lookup(lookup);
    let rule = Rule {
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "user.name".into(),
            data: "958".into(),
            ..Default::default()
        }),
        ..daemon_rule("a")
    };
    let subscribed = tokio::time::timeout(
        Duration::from_secs(5),
        svc.subscribe(Request::new(with_rules(vec![rule.clone()]))),
    )
    .await;
    assert!(subscribed.is_ok(), "Subscribe waited for the lookup");
    let commands = svc.daemon_commands();
    let (stream, _outbound) = commands.open_stream(None);
    commands.on_reply(stream.id(), &hello());
    let ServerMessage::SetRules { rules } = rx.try_recv().unwrap() else {
        panic!("expected SetRules");
    };
    assert!(rules[0].get("userNames").is_none(), "not known yet");
    // A snapshot meanwhile doesn't look the same uid up again.
    svc.subscribe(Request::new(with_rules(vec![rule.clone()])))
        .await
        .unwrap();
    assert_eq!(*calls.lock().unwrap(), 1, "looked up once");
    release.send(()).unwrap();
    let named = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::SetRules { rules } = rx.recv().await.unwrap() {
                if rules[0].get("userNames").is_some() {
                    return rules;
                }
            }
        }
    })
    .await
    .expect("the late name was never published");
    assert_eq!(
        named[0]["userNames"],
        serde_json::json!({ "958": "snitchwatch" })
    );
    svc.subscribe(Request::new(with_rules(vec![rule])))
        .await
        .unwrap();
    assert_eq!(*calls.lock().unwrap(), 1, "and not after it was found");
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

/// PR #106 review N1, the other HELLO order: the new stream says HELLO
/// first and adopts its snapshot, the old stream's late HELLO adopts the
/// same one and takes the list, then the old stream closes. The new stream,
/// current again, holds the list instead of seeing it withdrawn.
#[tokio::test]
async fn a_late_hello_after_the_new_streams_own_keeps_the_list_when_it_closes() {
    let (svc, _cache, _rx) = rules_service(DaemonTransport::Unix);
    let commands = svc.daemon_commands();
    svc.subscribe(Request::new(with_rules(vec![daemon_rule("old")])))
        .await
        .unwrap();
    let (old, _old_rx) = commands.open_stream(None);
    svc.subscribe(Request::new(with_rules(vec![daemon_rule("new")])))
        .await
        .unwrap();
    let (new, _new_rx) = commands.open_stream(None);
    commands.on_reply(new.id(), &hello());
    commands.on_reply(old.id(), &hello());
    assert!(cached(&svc).contains("new"));
    drop(old);
    assert!(!cached(&svc).is_unknown(), "withdrawn with the old stream");
    assert!(cached(&svc).contains("new"));
    // The new stream holds it now: its close withdraws it.
    drop(new);
    assert!(cached(&svc).is_unknown());
}

/// N1 holds only for a stream that adopted the snapshot: one that said
/// HELLO before it was staged doesn't get the list when the holder closes.
#[tokio::test]
async fn a_stream_that_never_adopted_the_snapshot_does_not_get_the_list() {
    let (svc, _cache, _rx) = rules_service(DaemonTransport::Unix);
    let commands = svc.daemon_commands();
    let (early, _early_rx) = commands.open_stream(None);
    commands.on_reply(early.id(), &hello());
    svc.subscribe(Request::new(with_rules(vec![daemon_rule("a")])))
        .await
        .unwrap();
    let (holder, _holder_rx) = commands.open_stream(None);
    commands.on_reply(holder.id(), &hello());
    assert!(cached(&svc).contains("a"));
    drop(holder);
    assert!(cached(&svc).is_unknown());
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

/// PR #106 review N1 on TCP, where another local process may hold any
/// stream: B adopts its own snapshot, A's later HELLO takes the list, then A
/// closes. B, current again, must not get A's list back: it is withdrawn,
/// and (N8) its subscribers see the empty `SetRules` and then a
/// `RulesNotShown` that no longer says `listed`.
#[tokio::test]
async fn a_closing_tcp_holder_withdraws_the_list_instead_of_handing_it_to_the_other_stream() {
    let (svc, _cache, mut rx) = rules_service(DaemonTransport::Tcp);
    let (a, b) = (
        Some(std::net::SocketAddr::from(([127, 0, 0, 1], 1001))),
        Some(std::net::SocketAddr::from(([127, 0, 0, 1], 1002))),
    );
    svc.rules.stage(a, vec![daemon_rule("from-a")]);
    svc.rules.stage(b, vec![daemon_rule("from-b")]);
    let commands = svc.daemon_commands();
    let (stream_a, _a_rx) = commands.open_stream(a);
    let (stream_b, _b_rx) = commands.open_stream(b);

    commands.on_reply(stream_b.id(), &hello());
    assert!(cached(&svc).contains("from-b"), "B adopted its own list");
    commands.on_reply(stream_a.id(), &hello());
    assert!(cached(&svc).contains("from-a"), "A's HELLO took the list");
    std::iter::from_fn(|| rx.try_recv().ok()).for_each(drop);

    drop(stream_a);

    assert!(cached(&svc).is_unknown(), "withdrawn, not handed to B");
    let after: Vec<ServerMessage> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        matches!(
            after.as_slice(),
            [
                ServerMessage::SetRules { rules },
                ServerMessage::RulesNotShown { listed: false, .. },
            ] if rules.is_empty()
        ),
        "the empty list, then a not-shown that is not `listed`: {after:?}"
    );
    drop(stream_b);
}
