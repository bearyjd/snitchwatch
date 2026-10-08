use super::*;
use futures_util::{SinkExt, StreamExt};
use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_bridge::ws_messages::{VerdictAction, VerdictDuration, VerdictScope};
use snitchwatch_proto::protocol::Connection;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;

#[test]
fn system_permissions_peer_helper() {
    let Some(dir) = std::env::var_os("SNITCHWATCH_TEST_PERMISSION_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let token_path = dir.join("auth/token");
    let role = std::env::var("SNITCHWATCH_TEST_PERMISSION_ROLE").unwrap();
    if role == "service" || role == "service-mismatch" {
        assert_eq!(unsafe { libc::geteuid() }, 65531);
        assert_eq!(unsafe { libc::getegid() }, 65531);
        if role == "service-mismatch" {
            let original = auth::read_token_file(&token_path).unwrap();
            for _ in 0..2 {
                let error =
                    auth::write_system_token_file(&Token::generate(), &token_path).unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                assert!(error
                    .to_string()
                    .contains("system token group did not inherit auth directory group"));
                assert!(original.matches(auth::read_token_file(&token_path).unwrap().as_str()));
                assert!(!dir
                    .join(format!("auth/.token.{}.tmp", std::process::id()))
                    .exists());
            }
            return;
        }
        auth::write_system_token_file(&Token::generate(), &token_path).unwrap();
        let metadata = std::fs::metadata(&token_path).unwrap();
        assert_eq!(
            (metadata.uid(), metadata.gid(), metadata.mode() & 0o777),
            (65531, 65533, 0o640)
        );
        return;
    }
    let member = role == "member";
    let gui = std::os::unix::net::UnixStream::connect(dir.join("bridge.sock"));
    let token = auth::read_token_file(&token_path);
    if member {
        assert!(gui.is_ok(), "UI-group member must be able to connect");
        assert_eq!(token.unwrap().as_str().len(), 64);
    } else {
        assert_eq!(
            gui.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            token.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
    let daemon = std::os::unix::net::UnixStream::connect(dir.join("opensnitchd.sock"));
    assert_eq!(
        daemon.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    for path in [
        dir.join("bridge.sock"),
        dir.join("opensnitchd.sock"),
        token_path,
    ] {
        assert_eq!(
            std::fs::remove_file(path).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}

#[tokio::test]
async fn service_token_and_socket_permissions_enforce_the_ui_group_across_identities() {
    use std::os::unix::process::CommandExt;
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("run this test as root to verify distinct service/UI identities");
        return;
    }
    // Match /run's native tmpfs semantics. Some rootless development
    // overlays report setgid directories but do not inherit their group.
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
    let gui_path = dir.path().join("bridge.sock");
    let daemon_path = dir.path().join("opensnitchd.sock");
    let _gui = UnixListener::bind(&gui_path).unwrap();
    let _daemon = UnixListener::bind(&daemon_path).unwrap();
    let auth_dir = dir.path().join("auth");
    std::fs::create_dir(&auth_dir).unwrap();
    for (path, uid, gid, mode) in [
        (&gui_path, 0, 65533, 0o660),
        (&daemon_path, 0, 0, 0o600),
        (&auth_dir, 65531, 65533, 0o2750),
    ] {
        use std::os::unix::ffi::OsStrExt;
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::chown(path_c.as_ptr(), uid, gid) }, 0);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    assert_eq!(
        std::fs::metadata(&auth_dir).unwrap().mode() & 0o7777,
        0o2750
    );
    for (role, uid, gid) in [
        ("service", 65531, 65531),
        ("member", 65534, 65533),
        ("nonmember", 65532, 65532),
        ("service-mismatch", 65531, 65531),
    ] {
        if role == "service-mismatch" {
            std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o750)).unwrap();
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "tests::system_permissions_peer_helper",
                "--nocapture",
            ])
            .env("SNITCHWATCH_TEST_PERMISSION_DIR", dir.path())
            .env("SNITCHWATCH_TEST_PERMISSION_ROLE", role);
        // Only async-signal-safe credential syscalls in the forked child.
        // Clear inherited groups before dropping root, so no membership
        // from the container runner can invalidate the negative checks.
        unsafe {
            child.pre_exec(move || {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(
            child.status().unwrap().success(),
            "{role} permission checks failed"
        );
        if role == "service-mismatch" {
            std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o2750)).unwrap();
        }
    }
    let token = std::fs::metadata(auth_dir.join("token")).unwrap();
    assert_eq!(
        (token.uid(), token.gid(), token.mode() & 0o777),
        (65531, 65533, 0o640)
    );
    assert_eq!(
        std::fs::metadata(&auth_dir).unwrap().mode() & 0o7777,
        0o2750
    );
    assert_eq!(std::fs::metadata(&gui_path).unwrap().mode() & 0o777, 0o660);
    assert_eq!(
        std::fs::metadata(&daemon_path).unwrap().mode() & 0o777,
        0o600
    );
}

// This helper runs in a fresh process so credentials can be changed safely,
// without mutating the credentials of a running multithreaded test suite.
#[test]
fn non_root_daemon_peer_helper() {
    use std::io::Read;
    let Some(path) = std::env::var_os("SNITCHWATCH_TEST_PEER_SOCKET") else {
        return;
    };
    assert_ne!(unsafe { libc::geteuid() }, 0);
    let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    assert_eq!(
        stream.read(&mut [0u8; 1]).unwrap(),
        0,
        "non-root peer must be disconnected"
    );
}

#[tokio::test]
async fn root_unix_incoming_rejects_non_root_and_keeps_accepting_every_peer() {
    use std::os::unix::process::CommandExt;
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("run this test as root to verify both credential classes");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let path = dir.path().join("grpc.sock");
    let mut incoming = RootUnixIncoming(UnixListener::bind(&path).unwrap());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
    for _ in 0..2 {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::non_root_daemon_peer_helper",
                "--nocapture",
            ])
            .env("SNITCHWATCH_TEST_PEER_SOCKET", &path)
            .uid(65534)
            .gid(65534)
            .spawn()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), incoming.next())
                .await
                .is_err()
        );
        assert!(child.wait().unwrap().success());
        let _root = UnixStream::connect(&path).await.unwrap();
        let accepted = tokio::time::timeout(Duration::from_secs(2), incoming.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(accepted.peer_cred().unwrap().uid(), 0);
    }
}

#[tokio::test]
async fn activated_unix_ask_rule_roundtrip_preserves_socket_ownership_and_modes() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("run this test as root to exercise the authorized daemon peer");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let gui_path = dir.path().join("bridge.sock");
    let grpc_path = dir.path().join("opensnitchd.sock");
    let gui_listener = UnixListener::bind(&gui_path).unwrap();
    let grpc_listener = UnixListener::bind(&grpc_path).unwrap();
    std::fs::set_permissions(&gui_path, std::fs::Permissions::from_mode(0o660)).unwrap();
    std::fs::set_permissions(&grpc_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let auth_dir = dir.path().join("auth");
    std::fs::create_dir(&auth_dir).unwrap();
    std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o2750)).unwrap();
    let before_gui = std::fs::metadata(&gui_path).unwrap();
    let before_grpc = std::fs::metadata(&grpc_path).unwrap();
    let before_dir = std::fs::metadata(dir.path()).unwrap();
    let token_path = auth_dir.join("token");
    let config = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: gui_path.clone(),
        cache_capacity: 64,
    };
    let bridge = run_with_incoming(
        config,
        GrpcEndpoint::Unix(grpc_path.clone()),
        RootUnixIncoming(grpc_listener),
        Some(gui_listener),
        Some(token_path.clone()),
        RunOptions::in_process(),
        ANSWER_TIMEOUT,
    )
    .await
    .unwrap();
    assert_eq!(bridge.grpc_endpoint, GrpcEndpoint::Unix(grpc_path.clone()));
    assert!(bridge.grpc_endpoint.tcp_addr().is_none());
    assert_eq!(
        std::fs::metadata(&token_path).unwrap().mode() & 0o777,
        0o640
    );
    assert_eq!(
        std::fs::metadata(&token_path).unwrap().gid(),
        std::fs::metadata(&auth_dir).unwrap().gid()
    );
    assert_eq!(
        std::fs::metadata(&auth_dir).unwrap().mode() & 0o7777,
        0o2750
    );

    let stream = UnixStream::connect(&gui_path).await.unwrap();
    let (mut gui, _) = tokio_tungstenite::client_async("ws://localhost/stream", stream)
        .await
        .unwrap();
    let token = auth::read_token_file(&token_path).unwrap();
    gui.send(Message::Text(token.as_str().to_owned()))
        .await
        .unwrap();
    let ack = tokio::time::timeout(Duration::from_secs(2), gui.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(ack, Message::Text(ref text) if matches!(serde_json::from_str::<ServerMessage>(text), Ok(ServerMessage::Authenticated { .. })))
    );

    let channel = tonic::transport::Endpoint::from_static("http://localhost")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = grpc_path.clone();
            async move {
                UnixStream::connect(path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .unwrap();
    let ask = tokio::spawn(async move {
        snitchwatch_proto::protocol::ui_client::UiClient::new(channel)
            .ask_rule(Connection {
                protocol: "tcp".into(),
                dst_host: "example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner()
    });
    let pending_id = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Message::Text(text) = gui.next().await.unwrap().unwrap() {
                if let Ok(ServerMessage::InsertConnectionRows { rows }) =
                    serde_json::from_str(&text)
                {
                    if let Some(row) = rows.into_iter().find(|row| row.action.is_none()) {
                        break row.id;
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    let verdict = ClientMessage::SetVerdict {
        row_id: pending_id,
        verdict: VerdictAction::Allow,
        scope: VerdictScope::ThisHost,
        duration: Some(VerdictDuration::Once),
        remember: None,
    };
    gui.send(Message::Text(serde_json::to_string(&verdict).unwrap()))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), ask)
            .await
            .unwrap()
            .unwrap()
            .action,
        "allow"
    );
    bridge.shutdown();
    tokio::task::yield_now().await;
    for (path, before) in [
        (&gui_path, before_gui),
        (&dir.path().join("opensnitchd.sock"), before_grpc),
    ] {
        let after = std::fs::metadata(path).unwrap();
        assert_eq!(
            (after.ino(), after.mode(), after.uid(), after.gid()),
            (before.ino(), before.mode(), before.uid(), before.gid())
        );
    }
    assert_eq!(
        std::fs::metadata(dir.path()).unwrap().mode(),
        before_dir.mode()
    );
}

#[tokio::test]
async fn run_binds_socket_and_grpc_port_and_shutdown_works() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg).await.expect("run failed");
    assert!(bridge.ws_socket_path.exists());
    assert!(bridge.ws_token_path.exists());
    assert!(bridge.grpc_endpoint.tcp_addr().unwrap().port() != 0);
    bridge.shutdown();
}

#[tokio::test]
async fn exposes_in_process_broadcast_and_inbound_handles() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg).await.expect("run failed");

    // Outbound: a subscriber gets the exact ServerMessage the bridge fans out.
    let mut rx = bridge.broadcast_tx.subscribe();
    let msg = ServerMessage::ClearConnectionRows;
    bridge.broadcast_tx.send(msg.clone()).unwrap();
    let got = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("no broadcast within timeout")
        .expect("broadcast channel closed");
    assert_eq!(got, msg);

    // Inbound: a UI-origin ClientMessage is accepted onto the upstream pump.
    bridge
        .inbound_tx
        .send(ClientMessage::Undo)
        .await
        .expect("inbound channel closed");

    bridge.shutdown();
}

#[tokio::test]
async fn verdict_broadcasts_an_updated_non_pending_row() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg).await.expect("run failed");
    let mut rx = bridge.broadcast_tx.subscribe();
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let transport = tokio::net::UnixStream::connect(&bridge.ws_socket_path)
        .await
        .unwrap();
    let (mut gui, _) = tokio_tungstenite::client_async("ws://localhost/stream", transport)
        .await
        .unwrap();
    gui.send(Message::Text(bridge.ws_token.as_str().into()))
        .await
        .unwrap();
    let ack = gui.next().await.unwrap().unwrap();
    assert!(matches!(ack, Message::Text(ref text) if text.contains("authenticated")));

    let grpc_addr = bridge.grpc_endpoint.tcp_addr().unwrap();
    let ask = tokio::spawn(async move {
        let mut daemon = MockOpensnitchd::connect(grpc_addr).await.unwrap();
        daemon
            .ask_rule(Connection {
                protocol: "tcp".into(),
                dst_host: "example.com".into(),
                dst_ip: "93.184.216.34".into(),
                dst_port: 443,
                process_path: "/usr/bin/curl".into(),
                ..Default::default()
            })
            .await
            .unwrap()
    });

    let pending_id = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let ServerMessage::InsertConnectionRows { rows } =
                rx.recv().await.expect("broadcast channel closed")
            {
                if let Some(row) = rows.into_iter().find(|row| row.action.is_none()) {
                    break row.id;
                }
            }
        }
    })
    .await
    .expect("pending AskRule row was not broadcast");

    bridge
        .inbound_tx
        .send(ClientMessage::SetVerdict {
            row_id: pending_id.clone(),
            verdict: VerdictAction::Allow,
            scope: VerdictScope::ThisHost,
            duration: Some(VerdictDuration::Once),
            remember: None,
        })
        .await
        .expect("inbound channel closed");

    let updated = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let ServerMessage::UpdateConnectionRows { rows } =
                rx.recv().await.expect("broadcast channel closed")
            {
                if let Some(row) = rows.into_iter().find(|row| row.id == pending_id) {
                    break row;
                }
            }
        }
    })
    .await
    .expect("verdict did not broadcast a row update");
    assert_eq!(updated.action.as_deref(), Some("allow"));

    let rule = ask.await.expect("AskRule task panicked");
    assert_eq!(rule.action, "allow");
    bridge.shutdown();
}

#[tokio::test]
async fn request_snapshot_rebroadcasts_bridge_owned_state() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg).await.expect("run failed");
    let mut rx = bridge.broadcast_tx.subscribe();

    bridge
        .inbound_tx
        .send(ClientMessage::RequestSnapshot)
        .await
        .expect("inbound channel closed");

    // Expected snapshot sequence for an empty bridge: a connections clear
    // (no insert — the cache is empty), then blocklists, profiles, and
    // the current tray value. The latter lets a GUI that subscribed after
    // a state transition render the service-owned shell state correctly.
    // Ignore unrelated interleavings (e.g. traffic pump output) but bound
    // the wait so a missing snapshot fails rather than hangs.
    let mut saw_clear = false;
    let mut saw_blocklists = false;
    let mut saw_profiles = false;
    let mut saw_tray = false;
    let mut saw_pause = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while !(saw_clear && saw_blocklists && saw_profiles && saw_tray && saw_pause) {
        let msg = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("snapshot messages not re-broadcast within timeout")
            .expect("broadcast channel closed");
        match msg {
            ServerMessage::ClearConnectionRows => saw_clear = true,
            ServerMessage::SetBlocklists { .. } => saw_blocklists = true,
            ServerMessage::SetProfiles { .. } => saw_profiles = true,
            ServerMessage::TrayState {
                state: TrayState::Idle,
            } => saw_tray = true,
            // A GUI that was away when a pause ended learns it here.
            ServerMessage::FilterPauseState {
                paused: false,
                expires_at_unix_ms: None,
            } => saw_pause = true,
            _ => {}
        }
    }
    bridge.shutdown();
}

#[tokio::test]
async fn synthetic_connection_activity_is_rebroadcast_as_traffic_events() {
    use snitchwatch_bridge::ws_messages::ConnectionRow;

    let dir = tempfile::tempdir().unwrap();
    let cfg = BridgeConfig {
        grpc_bind: "127.0.0.1:0".parse().unwrap(),
        ws_socket_path: dir.path().join("bridge.sock"),
        cache_capacity: 64,
    };
    let bridge = run(cfg).await.expect("run failed");
    let mut rx = bridge.broadcast_tx.subscribe();

    // Simulate what `UiService::ask_rule` broadcasts on a real connection
    // (a synthetic row with non-zero byte counters, since production
    // `ask_rule` rows start at zero — this exercises the pump's mapping
    // end-to-end regardless of what today's actual producer sends).
    let row = ConnectionRow {
        id: "ask-1".into(),
        process: "curl".into(),
        process_path: Some("/usr/bin/curl".into()),
        dst_host: "example.com".into(),
        dst_ip: "93.184.216.34".into(),
        dst_port: 443,
        protocol: "tcp".into(),
        direction: "outgoing".into(),
        action: None,
        bytes_sent: 1234,
        bytes_received: 5678,
        started_at_ms: 0,
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
    };
    bridge
        .broadcast_tx
        .send(ServerMessage::InsertConnectionRows {
            rows: vec![row.clone()],
        })
        .expect("broadcast send failed");

    // First: the original InsertConnectionRows, echoed to every subscriber
    // (including this test's own, exactly like a browser WS client).
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("no broadcast within timeout")
        .expect("broadcast channel closed");
    assert_eq!(
        first,
        ServerMessage::InsertConnectionRows { rows: vec![row] }
    );

    // Second: the traffic pump's derived TrafficEvents, mapping
    // bytes_sent -> bytesOut and bytes_received -> bytesIn.
    let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("no TrafficEvents broadcast within timeout")
        .expect("broadcast channel closed");
    match second {
        ServerMessage::TrafficEvents { events } => {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].bytes_in, 5678);
            assert_eq!(events[0].bytes_out, 1234);
        }
        other => panic!("expected TrafficEvents, got {other:?}"),
    }

    bridge.shutdown();
}
