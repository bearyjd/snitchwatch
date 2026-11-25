//! Test rig for the control gate: a real store in a tempdir, a capture on the
//! shared generation, and small helpers. Test-only.

use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use rusqlite::Connection as Db;
use snitchwatch_proto::protocol::{Connection, Event, Operator, Rule};

use super::admission::AdmissionConfig;
use super::capture::{Capture, CaptureConfig};
use super::durable::tests::{engine_at, manual_clock};
use super::lifecycle::{AcceptanceSeq, Generation};
use super::pins::PinRegistry;
use super::test_support::{state_dir, tempdir};
use super::{enable, locate, Consent, Location, Mode};

pub(crate) fn config() -> CaptureConfig {
    let mut admission = AdmissionConfig::standard();
    admission.process_rate = 1_000_000;
    admission.process_burst = 1_000_000;
    admission.global_rate = 1_000_000;
    admission.global_burst = 1_000_000;
    CaptureConfig {
        queue_max_bytes: super::writer::QUEUE_MAX_BYTES,
        queue_max_count: super::writer::QUEUE_MAX_COUNT,
        admission,
        heartbeat_every: std::time::Duration::from_secs(3600),
        pause_min: std::time::Duration::from_millis(500),
        pause_max: std::time::Duration::from_secs(60),
        expiry_every: std::time::Duration::from_secs(600),
        retention_days: 30,
    }
}

pub(crate) fn consent() -> Consent {
    Consent {
        text_id: "t".into(),
        accepted_ms: 1,
    }
}

pub(crate) struct Rig {
    pub root: tempfile::TempDir,
    pub location: Location,
    pub registry: Arc<PinRegistry>,
    pub clock: Arc<AtomicI64>,
    pub generation: Arc<Generation>,
    pub acceptance: Arc<AcceptanceSeq>,
    pub capture: Capture,
}

/// An enabled store with a running capture on a shared generation.
pub(crate) fn rig() -> Rig {
    let root = tempdir();
    let state = state_dir(&root);
    let location = locate(Mode::User, Some(&state)).unwrap();
    enable(&location, consent()).unwrap();
    let (clock, read) = manual_clock(10_000);
    let registry = Arc::new(PinRegistry::with_clock(read));
    let generation = Arc::new(Generation::from_epoch(7));
    let acceptance = Arc::new(AcceptanceSeq::default());
    let capture = Capture::spawn_shared(
        engine_at(&location),
        registry.clone(),
        config(),
        generation.clone(),
        acceptance.clone(),
    )
    .unwrap();
    Rig {
        root,
        location,
        registry,
        clock,
        generation,
        acceptance,
        capture,
    }
}

pub(crate) fn conn(path: &str, host: &str) -> Connection {
    Connection {
        protocol: "tcp".into(),
        src_ip: "192.168.1.10".into(),
        src_port: 51544,
        dst_ip: "140.82.121.4".into(),
        dst_host: host.into(),
        dst_port: 443,
        user_id: 1000,
        process_id: 4242,
        process_path: path.into(),
        ..Connection::default()
    }
}

pub(crate) fn rule(name: &str, action: &str) -> Rule {
    Rule {
        name: name.into(),
        action: action.into(),
        duration: "always".into(),
        enabled: true,
        operator: Some(Operator {
            r#type: "simple".into(),
            operand: "process.path".into(),
            data: "/usr/bin/curl".into(),
            ..Operator::default()
        }),
        ..Rule::default()
    }
}

pub(crate) fn event(path: &str, host: &str, ms: i64) -> Event {
    Event {
        time: String::new(),
        connection: Some(conn(path, host)),
        rule: Some(rule("allow-web", "allow")),
        unixnano: ms * 1_000_000,
    }
}

pub(crate) fn count(location: &Location, sql: &str) -> i64 {
    let db = Db::open_with_flags(
        location.db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}
