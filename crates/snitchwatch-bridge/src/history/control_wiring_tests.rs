//! PR 10: the gRPC path asks the slot on every call, so a ping after a clear
//! or a disable reaches the capture that exists now, never a stopped one.

use std::sync::Arc;
use std::time::Duration;

use snitchwatch_proto::protocol::ui_server::Ui;
use snitchwatch_proto::protocol::{PingRequest, Statistics};
use tokio::sync::{broadcast, Mutex};
use tonic::Request;

use crate::cache::connections::ConnectionCache;
use crate::filter_pause::FilterPause;
use crate::grpc_server::UiService;
use crate::notice::NoticeBus;
use crate::tray_state::TrayStatePublisher;

use super::control::Limits;
use super::control_support::{count, event};
use super::control_tests::{gate_rig, wait_for};

fn service(slot: super::control::CaptureSlot) -> UiService {
    let (tx, _rx) = broadcast::channel(64);
    UiService::new(
        Arc::new(Mutex::new(ConnectionCache::new(64))),
        tx,
        Arc::new(TrayStatePublisher::new()),
        Arc::new(NoticeBus::new()),
        Arc::new(FilterPause::new()),
    )
    .with_history_slot(slot)
}

async fn ping(svc: &UiService, host: &str) {
    let mut e = event("/usr/bin/curl", host, 1_700_000_000_000);
    e.unixnano = 1_700_000_000_000_000_000;
    svc.ping(Request::new(PingRequest {
        id: 1,
        stats: Some(Statistics {
            events: vec![e],
            uptime: 5,
            rule_hits: 1,
            ..Default::default()
        }),
    }))
    .await
    .unwrap();
}

#[tokio::test]
async fn a_ping_after_a_clear_lands_in_the_new_store_and_one_after_disable_in_none() {
    let rig = gate_rig(Limits::default());
    let svc = service(rig.gate.slot());
    ping(&svc, "before.example").await;
    let rows = || count(&rig.location, "SELECT COUNT(*) FROM events");
    wait_for("the first row", || rows() == 1);
    let token = rig.gate.preview().unwrap().token;
    let gate = rig.gate.clone();
    tokio::task::spawn_blocking(move || gate.clear(token))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows(), 0);
    ping(&svc, "after.example").await;
    wait_for("the row after the clear", || rows() == 1);
    assert_eq!(
        count(
            &rig.location,
            "SELECT COUNT(*) FROM events WHERE dst_host = 'after.example'"
        ),
        1
    );
    let gate = rig.gate.clone();
    tokio::task::spawn_blocking(move || gate.disable())
        .await
        .unwrap()
        .unwrap();
    ping(&svc, "off.example").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rows(), 1, "nothing recorded while off");
}
