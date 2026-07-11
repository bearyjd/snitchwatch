//! Manual Task 7 probe: fire a single synthetic `AskRule` at a *running*
//! `snitchwatch-kirigami` process's gRPC port, to trigger the
//! `PendingDecisionSheet` for a real, human-eyeballed fullscreen-focus test.
//!
//! This is throwaway tooling for one manual verification step in
//! `docs/superpowers/plans/2026-07-04-kirigami-shell-rewrite.md` (Task 7) —
//! it is not part of the automated test suite and never touches a real
//! `opensnitchd`, per this repo's testing conventions.
//!
//! Usage:
//!   cargo run -p mock-opensnitchd --example fire_ask_rule -- [bridge_addr]
//!
//! `bridge_addr` defaults to `127.0.0.1:50051` (the kirigami shell's default
//! `SNITCHWATCH_GRPC_BIND`). Run this against an already-running
//! `snitchwatch-kirigami` binary.

use mock_opensnitchd::MockOpensnitchd;
use snitchwatch_proto::protocol::Connection;

#[tokio::main]
async fn main() {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:50051".to_string())
        .parse()
        .expect("invalid bridge address");

    println!("Connecting to bridge at {addr}...");
    let mut mock = MockOpensnitchd::connect(addr)
        .await
        .expect("failed to connect — is snitchwatch-kirigami running?");

    mock.subscribe("fire_ask_rule-manual-probe")
        .await
        .expect("subscribe failed");

    let conn = Connection {
        protocol: "tcp".to_string(),
        src_ip: "127.0.0.1".to_string(),
        src_port: 51234,
        dst_ip: "93.184.216.34".to_string(),
        dst_host: "manual-fullscreen-probe.example".to_string(),
        dst_port: 443,
        user_id: 1000,
        process_id: std::process::id(),
        process_path: "/usr/bin/manual-fullscreen-probe".to_string(),
        process_cwd: "/home/user".to_string(),
        process_args: vec!["manual-fullscreen-probe".to_string()],
        process_env: Default::default(),
        process_checksums: Default::default(),
        process_tree: vec![],
    };

    println!("Sending AskRule — watch the shell now for the pending-decision prompt...");
    let rule = mock.ask_rule(conn).await.expect("ask_rule RPC failed");
    println!("Bridge resolved with rule: {rule:?}");
}
