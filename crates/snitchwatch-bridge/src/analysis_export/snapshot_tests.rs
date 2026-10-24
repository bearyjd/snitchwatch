use super::snapshot::*;
use super::*;
use crate::ws_messages::{AutoAnswer, ConnectionRow};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

fn control() -> ExportControl {
    ExportControl {
        deadline: Instant::now() + Duration::from_secs(30),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

fn row(time: i64) -> ConnectionRow {
    ConnectionRow {
        id: "excluded-id".into(),
        process: "excluded-process".into(),
        process_path: Some("/usr/bin/example".into()),
        dst_host: "example.invalid".into(),
        dst_ip: "192.0.2.1".into(),
        dst_port: 443,
        protocol: "tcp".into(),
        direction: "outgoing".into(),
        action: None,
        bytes_sent: u64::MAX,
        bytes_received: u64::MAX,
        started_at_ms: time,
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: Some(-1),
        deferred: false,
        decided_by_default: false,
    }
}

#[test]
fn snapshot_ignores_unused_fields_and_reports_bounds() {
    let mut rows: Vec<_> = (0..10_005).map(row).collect();
    // Newest insertion, not timestamp order; pending rows may exceed capacity.
    rows[10_004].started_at_ms = 7;
    let snapshot = capture(&rows, 123, &control()).unwrap();
    assert_eq!(snapshot.source.retained, 10_005);
    assert_eq!(snapshot.source.inspected, 10_000);
    assert_eq!(snapshot.source.unexamined, 5);
    assert_eq!(snapshot.source.accepted_snapshot, 10_000);
    assert_eq!(snapshot.rows.first().unwrap().started_at_ms, 7);
    assert_eq!(snapshot.rows.last().unwrap().started_at_ms, 5);
    assert_eq!(snapshot.captured_at_ms, 123);

    let mut huge = row(1);
    huge.id = "i".repeat(100 * 1024 * 1024);
    huge.direction = "d".repeat(100 * 1024 * 1024);
    huge.process = "ignored".repeat(1000); // Path is present: label isn't needed.
    huge.dst_ip = "ignored".repeat(1000); // Host is present: IP isn't needed.
    let snapshot = capture(&[huge], 123, &control()).unwrap();
    assert_eq!(snapshot.source.accepted_snapshot, 1);
    assert_eq!(
        snapshot.source.copied_input_bytes,
        "/usr/bin/example".len() + "example.invalid".len()
    );
    assert_eq!(snapshot.source.omitted_required_field, 0);
}

#[test]
fn required_field_limit_is_bytes_and_only_needed_inputs_are_checked() {
    for field in [
        "path",
        "process",
        "destination",
        "protocol",
        "action",
        "rule",
    ] {
        for (len, accepted) in [(4096, 1), (4097, 0)] {
            let mut input = row(1);
            let value = "x".repeat(len);
            match field {
                "path" => input.process_path = Some(value),
                "process" => {
                    input.process_path = None;
                    input.process = value;
                }
                "destination" => input.dst_host = value,
                "protocol" => input.protocol = value,
                "action" => input.action = Some(value),
                "rule" => input.matched_rule = Some(value),
                _ => unreachable!(),
            }
            let snapshot = capture(&[input], 123, &control()).unwrap();
            assert_eq!(snapshot.source.accepted_snapshot, accepted, "{field} {len}");
            assert_eq!(
                snapshot.source.omitted_required_field,
                1 - accepted,
                "{field} {len}"
            );
            assert_eq!(
                snapshot.source.examined_range,
                Some(ObservedRange {
                    first_ms: 1,
                    last_ms: 1
                })
            );
        }
    }
    let mut unicode = row(1);
    unicode.process_path = Some("é".repeat(2049));
    assert_eq!(
        capture(&[unicode], 123, &control())
            .unwrap()
            .source
            .omitted_required_field,
        1
    );
}

#[test]
fn copied_budget_accepts_exact_limit_and_continues_after_omissions() {
    let large = || {
        let mut r = row(10);
        r.process_path = Some("x".repeat(4096));
        r.dst_host.clear();
        r.dst_ip.clear();
        r
    };
    let exact: Vec<_> = (0..2048).map(|_| large()).collect();
    let snapshot = capture(&exact, 123, &control()).unwrap();
    assert_eq!(snapshot.source.copied_input_bytes, 8_388_608);
    assert_eq!(snapshot.source.accepted_snapshot, 2048);
    let mut one_more = row(20);
    one_more.process_path = Some("x".into());
    one_more.dst_host.clear();
    one_more.dst_ip.clear();
    // Visited after the 2048 full fields; a zero-byte row still fits afterwards.
    let mut zero = row(30);
    zero.process_path = Some(String::new());
    zero.dst_host.clear();
    zero.dst_ip.clear();
    let mut inputs = vec![zero, one_more];
    inputs.extend(exact);
    let snapshot = capture(&inputs, 123, &control()).unwrap();
    assert_eq!(snapshot.source.inspected, 2050);
    assert_eq!(snapshot.source.accepted_snapshot, 2049);
    assert_eq!(snapshot.source.omitted_copy_budget, 1);
    assert_eq!(snapshot.source.copied_input_bytes, 8_388_608);
    assert_eq!(
        snapshot.source.examined_range,
        Some(ObservedRange {
            first_ms: 10,
            last_ms: 30
        })
    );
    assert_eq!(checked_copy_total(usize::MAX, 1), None);
    assert_eq!(checked_copy_total(8_388_608, 0), Some(8_388_608));
    assert_eq!(checked_copy_total(8_388_608, 1), None);
}

#[test]
fn snapshot_coverage_is_not_selection() {
    let mut inputs = vec![
        row(-1),
        row(MAX_TIMESTAMP_MS + 1),
        row(0),
        row(MAX_TIMESTAMP_MS),
    ];
    let mut oversized = row(50);
    oversized.process_path = Some("x".repeat(4097));
    inputs.push(oversized);
    let snapshot = capture(&inputs, 123, &control()).unwrap();
    assert_eq!(snapshot.source.inspected, 5);
    assert_eq!(snapshot.source.accepted_snapshot, 2);
    assert_eq!(snapshot.source.omitted_invalid_timestamp, 2);
    assert_eq!(snapshot.source.omitted_required_field, 1);
    assert_eq!(
        snapshot.source.examined_range,
        Some(ObservedRange {
            first_ms: 0,
            last_ms: MAX_TIMESTAMP_MS
        })
    );
    // The older invalid row is unexamined; its invalidity must not be reported.
    let mut inputs = vec![row(-1)];
    inputs.extend((0..10_000).map(row));
    let snapshot = capture(&inputs, 123, &control()).unwrap();
    assert_eq!(snapshot.source.unexamined, 1);
    assert_eq!(snapshot.source.omitted_invalid_timestamp, 0);
    assert_eq!(
        snapshot.source.examined_range,
        Some(ObservedRange {
            first_ms: 0,
            last_ms: 9999
        })
    );
    assert_eq!(
        capture(&[], 123, &control()).unwrap().source.examined_range,
        None
    );
}

#[test]
fn projection_uses_finite_classifications_and_one_private_identity() {
    let mut input = row(1);
    input.process_path = None;
    input.process = "private-label".into();
    input.dst_host.clear();
    input.dst_ip = "192.0.2.1".into();
    input.protocol = "hostile-protocol".into();
    input.action = Some("hostile-action".into());
    input.matched_rule = Some("hostile-rule".into());
    input.auto_answer = Some(AutoAnswer::Unknown);
    let snapshot = capture(&[input], 123, &control()).unwrap();
    let projected = &snapshot.rows[0];
    assert!(
        matches!(&projected.app, AppIdentity::MissingPath { process } if process == "private-label")
    );
    assert_eq!(projected.destination, "192.0.2.1");
    assert_eq!(projected.protocol, ExportProtocol::Unknown);
    assert_eq!(projected.verdict, ExportVerdict::Unknown);
    assert_eq!(projected.rule_class, ExportRuleClass::Other);
    assert_eq!(projected.auto_answer, ExportAutoAnswer::Unknown);
    let cases = [
        (None, false, false, ExportVerdict::Pending),
        (None, true, false, ExportVerdict::Unknown),
        (None, false, true, ExportVerdict::Unknown),
        (Some("allow"), false, false, ExportVerdict::Allow),
        (Some("deny"), false, false, ExportVerdict::Deny),
        (Some("blocklist"), false, false, ExportVerdict::Blocklist),
    ];
    for (action, deferred, default, verdict) in cases {
        let mut input = row(1);
        input.action = action.map(str::to_owned);
        input.deferred = deferred;
        input.decided_by_default = default;
        assert_eq!(
            capture(&[input], 123, &control()).unwrap().rows[0].verdict,
            verdict
        );
    }
    for (name, class) in [
        (None, ExportRuleClass::Unknown),
        (Some("snitchwatch-allow-x"), ExportRuleClass::Interactive),
        (Some("z00-blocklist:x"), ExportRuleClass::Blocklist),
        (Some("900-blocklist:x"), ExportRuleClass::Blocklist),
        (Some("850-profile:x"), ExportRuleClass::Profile),
        (Some("000-snitchwatch-x"), ExportRuleClass::Packaged),
        (Some("snitchwatch-default-x"), ExportRuleClass::Curated),
        (Some("anything"), ExportRuleClass::Other),
    ] {
        let mut input = row(1);
        input.matched_rule = name.map(str::to_owned);
        assert_eq!(
            capture(&[input], 123, &control()).unwrap().rows[0].rule_class,
            class
        );
    }
    let mut input = row(1);
    input.decided_by_default = true;
    input.matched_rule = Some("anything".into());
    assert_eq!(
        capture(&[input], 123, &control()).unwrap().rows[0].rule_class,
        ExportRuleClass::Default
    );
}

#[test]
fn cancellation_deadline_and_errors_are_static() {
    let c = control();
    c.cancelled.store(true, Ordering::Relaxed);
    assert!(matches!(capture(&[], 123, &c), Err(ExportError::Cancelled)));
    let c = ExportControl {
        deadline: Instant::now() - Duration::from_millis(1),
        ..control()
    };
    assert!(matches!(
        capture(&[row(1)], 123, &c),
        Err(ExportError::TimedOut)
    ));
    assert!(matches!(
        capture(&[], -1, &control()),
        Err(ExportError::InvalidRequest)
    ));
    for error in [
        ExportError::InvalidRequest,
        ExportError::UnsupportedVersion,
        ExportError::Busy,
        ExportError::Cancelled,
        ExportError::TimedOut,
        ExportError::Unavailable,
        ExportError::Internal,
        ExportError::Collision,
        ExportError::TooLarge,
    ] {
        assert!(!format!("{error:?}: {error}").contains("private"));
        assert!(error
            .to_string()
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_'));
    }
}

#[test]
fn filter_contract_preserves_exact_selectors_and_rejects_invalid_values() {
    let filters: ExportFilters = serde_json::from_str(
        r#"{"window":{"fromMs":0,"toMs":1},"app":{"kind":"path","value":""},"verdicts":[]}"#,
    )
    .unwrap();
    assert!(filters.validate().is_ok());
    assert!(matches!(filters.app, Some(AppSelector::Path { ref value }) if value.is_empty()));
    for invalid in [
        r#"{"unknown":1}"#,
        r#"{"app":{"kind":"missingPath","value":"leak"}}"#,
        r#"{"window":{"fromMs":0,"fromMs":1,"toMs":2}}"#,
        r#"{"verdicts":["alien"]}"#,
    ] {
        assert!(serde_json::from_str::<ExportFilters>(invalid).is_err());
    }
    for invalid in [
        r#"{"window":{"fromMs":-1,"toMs":1}}"#,
        r#"{"window":{"fromMs":1,"toMs":1}}"#,
        r#"{"window":{"fromMs":0,"toMs":253402300800000}}"#,
        r#"{"verdicts":["allow","allow"]}"#,
        r#"{"verdicts":["allow","deny","blocklist","pending","unknown","allow"]}"#,
    ] {
        assert!(serde_json::from_str::<ExportFilters>(invalid)
            .unwrap()
            .validate()
            .is_err());
    }
    let filters = ExportFilters {
        app: Some(AppSelector::Path {
            value: "x".repeat(4097),
        }),
        ..ExportFilters::default()
    };
    assert_eq!(filters.validate(), Err(ExportError::InvalidRequest));
}

#[test]
fn source_shape_guard_lists_all_seventeen_fields() {
    let source = include_str!("../ws_messages/verdict_wire.rs");
    let body = source
        .split("pub struct ConnectionRow {")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    let fields: Vec<_> = body
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("pub ")
                .and_then(|s| s.split_once(':').map(|(name, _)| name))
        })
        .collect();
    assert_eq!(
        fields,
        [
            "id",
            "process",
            "process_path",
            "dst_host",
            "dst_ip",
            "dst_port",
            "protocol",
            "direction",
            "action",
            "bytes_sent",
            "bytes_received",
            "started_at_ms",
            "matched_rule",
            "auto_answer",
            "answer_deadline_ms",
            "deferred",
            "decided_by_default"
        ]
    );
}

#[test]
fn private_inputs_have_no_debug_or_serialization_implementation() {
    // Deriving either trait on private inputs makes the inference ambiguous.
    trait NoPrivateDebug<A> {
        fn check() {}
    }
    impl<T: ?Sized> NoPrivateDebug<()> for T {}
    struct DebugMarker;
    impl<T: ?Sized + std::fmt::Debug> NoPrivateDebug<DebugMarker> for T {}
    let _ = <Snapshot as NoPrivateDebug<_>>::check;
    let _ = <SnapshotRow as NoPrivateDebug<_>>::check;
    let _ = <AppIdentity as NoPrivateDebug<_>>::check;
    let _ = <ExportFilters as NoPrivateDebug<_>>::check;
    let _ = <AppSelector as NoPrivateDebug<_>>::check;
    trait NoPrivateSerialization<A> {
        fn check() {}
    }
    impl<T: ?Sized> NoPrivateSerialization<()> for T {}
    struct SerializeMarker;
    impl<T: ?Sized + serde::Serialize> NoPrivateSerialization<SerializeMarker> for T {}
    let _ = <Snapshot as NoPrivateSerialization<_>>::check;
    let _ = <SnapshotRow as NoPrivateSerialization<_>>::check;
    let _ = <AppIdentity as NoPrivateSerialization<_>>::check;
}

#[test]
fn protocol_and_auto_answer_allowlists_do_not_retain_source_text() {
    for (source, expected) in [
        ("tcp", ExportProtocol::Tcp),
        ("udp", ExportProtocol::Udp),
        ("icmp", ExportProtocol::Icmp),
        ("icmpv6", ExportProtocol::Icmpv6),
        ("TCP", ExportProtocol::Unknown),
    ] {
        let mut input = row(1);
        input.protocol = source.into();
        assert_eq!(
            capture(&[input], 123, &control()).unwrap().rows[0].protocol,
            expected
        );
    }
    for (source, expected) in [
        (None, ExportAutoAnswer::None),
        (
            Some(AutoAnswer::FilterPaused),
            ExportAutoAnswer::FilterPaused,
        ),
        (Some(AutoAnswer::NoAnswer), ExportAutoAnswer::NoAnswer),
        (Some(AutoAnswer::Unknown), ExportAutoAnswer::Unknown),
    ] {
        let mut input = row(1);
        input.auto_answer = source;
        assert_eq!(
            capture(&[input], 123, &control()).unwrap().rows[0].auto_answer,
            expected
        );
    }
}

#[test]
fn empty_event_rule_is_unknown_unless_explicitly_decided_by_default() {
    use crate::translator::connection::event_to_row;
    use snitchwatch_proto::protocol::{Connection, Event, Rule};

    // Stock daemon events can report a real, unnamed rule without the default marker.
    let mut event = Event {
        connection: Some(Connection {
            protocol: "tcp".into(),
            dst_host: "example.invalid".into(),
            process_path: "/usr/bin/example".into(),
            ..Default::default()
        }),
        rule: Some(Rule {
            action: "deny".into(),
            duration: "always".into(),
            ..Default::default()
        }),
        unixnano: 1_000_000,
        ..Default::default()
    };
    let mut translated = event_to_row(&event, |_| Some(false)).unwrap();
    assert_eq!(translated.matched_rule.as_deref(), Some(""));
    assert!(!translated.decided_by_default);
    assert_eq!(
        capture(std::slice::from_ref(&translated), 123, &control())
            .unwrap()
            .rows[0]
            .rule_class,
        ExportRuleClass::Unknown
    );

    // An explicit default flag takes precedence even if an empty name is present.
    translated.decided_by_default = true;
    assert_eq!(
        capture(&[translated], 123, &control()).unwrap().rows[0].rule_class,
        ExportRuleClass::Default
    );
    event.rule.as_mut().unwrap().description = crate::daemon_contract::DEFAULT_ACTION_MARKER.into();
    let translated = event_to_row(&event, |_| Some(false)).unwrap();
    assert!(translated.decided_by_default);
    assert_eq!(translated.matched_rule, None);
    assert_eq!(
        capture(&[translated], 123, &control()).unwrap().rows[0].rule_class,
        ExportRuleClass::Default
    );
}
