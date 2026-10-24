use super::*;
use crate::ws_messages::{AutoAnswer, ConnectionRow};
use ring::hmac::{self, Key};
use serde_json::{json, Value};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::{Duration, Instant};

fn control() -> ExportControl {
    ExportControl {
        deadline: Instant::now() + Duration::from_secs(30),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

fn key() -> Key {
    Key::new(hmac::HMAC_SHA256, &[7; 32])
}

fn row(time: i64) -> ConnectionRow {
    ConnectionRow {
        id: "RAW-ID-CANARY".into(),
        process: "FAKE-PROCESS-CANARY".into(),
        process_path: Some("/usr/bin/example".into()),
        dst_host: "example.invalid".into(),
        dst_ip: "192.0.2.1".into(),
        dst_port: 443,
        protocol: "tcp".into(),
        direction: "RAW-DIRECTION-CANARY".into(),
        action: Some("allow".into()),
        bytes_sent: 123_456_789,
        bytes_received: 987_654_321,
        started_at_ms: time,
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: Some(999),
        deferred: false,
        decided_by_default: false,
    }
}

fn document(rows: &[ConnectionRow], filters: &ExportFilters) -> ExportDocument {
    let control = control();
    build(
        &capture(rows, 1234, &control).unwrap(),
        filters,
        &key(),
        &control,
    )
    .unwrap()
}

fn value(rows: &[ConnectionRow], filters: &ExportFilters) -> Value {
    serde_json::to_value(document(rows, filters)).unwrap()
}

#[test]
fn filters_are_exact_before_grouping() {
    let mut rows: Vec<_> = [9, 10, 11, 19, 20].into_iter().map(row).collect();
    for path in [
        Some("/usr/bin/example-extra"),
        Some("/USR/bin/example"),
        Some(""),
        None,
    ] {
        let mut item = row(11);
        item.process_path = path.map(str::to_owned);
        rows.push(item);
    }
    let mut unknown = row(11);
    unknown.action = None;
    unknown.deferred = true;
    rows.push(unknown);
    let mut pending = row(11);
    pending.action = None;
    rows.push(pending);
    let mut filters = ExportFilters {
        window: Some(TimeWindow {
            from_ms: 10,
            to_ms: 20,
        }),
        app: Some(AppSelector::Path {
            value: "/usr/bin/example".into(),
        }),
        verdicts: Some(vec![ExportVerdict::Allow]),
    };
    let output = value(&rows, &filters);
    assert_eq!(output["counts"]["selected"], 3);
    assert_eq!(output["groups"].as_array().unwrap().len(), 1);
    assert_eq!(output["groups"][0]["count"], 3);
    assert_eq!(output["groups"][0]["firstObservedAtMs"], 10);
    assert_eq!(output["groups"][0]["lastObservedAtMs"], 19);
    assert_eq!(
        output["filterKinds"],
        json!({"timeWindow":true,"application":true,"verdicts":true})
    );
    assert!(!output.to_string().contains("/usr/bin/example"));
    filters.verdicts = Some(vec![ExportVerdict::Unknown]);
    assert_eq!(value(&rows, &filters)["counts"]["selected"], 1);
    filters.verdicts = Some(vec![ExportVerdict::Pending]);
    assert_eq!(value(&rows, &filters)["counts"]["selected"], 1);
    filters.verdicts = Some(vec![]);
    assert_eq!(value(&rows, &filters)["counts"]["selected"], 0);
    filters.verdicts = None;
    filters.app = Some(AppSelector::MissingPath);
    assert_eq!(value(&rows, &filters)["counts"]["selected"], 1);
    filters.app = None;
    assert_eq!(value(&rows, &filters)["counts"]["selected"], 9);
    let empty = value(&[], &ExportFilters::default());
    assert_eq!(empty["counts"]["emitted"], 0);
    assert_eq!(empty["coverage"]["examinedTimestampRange"], Value::Null);
    assert_eq!(empty["coverage"]["completeness"], "incomplete");
}

#[test]
fn destination_fallback_and_app_identity() {
    let mut rows = Vec::new();
    for (host, ip) in [("", "192.0.2.1"), ("192.0.2.1", "203.0.113.99")] {
        let mut item = row(1);
        item.dst_host = host.into();
        item.dst_ip = ip.into();
        rows.push(item);
    }
    for host in ["2001:db8::1", "2001:0db8:0000:0000:0000:0000:0000:0001"] {
        let mut item = row(1);
        item.dst_host = host.into();
        rows.push(item);
    }
    let mut ipv6_fallback = row(1);
    ipv6_fallback.dst_host.clear();
    ipv6_fallback.dst_ip = "2001:db8::1".into();
    rows.push(ipv6_fallback);
    for host in [
        "host.example",
        "HOST.example",
        "host.example.",
        "雪.example",
    ] {
        let mut item = row(1);
        item.dst_host = host.into();
        rows.push(item);
    }
    let output = value(&rows, &ExportFilters::default());
    let groups = output["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 6);
    assert_eq!(
        groups
            .iter()
            .map(|g| g["count"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [3, 2, 1, 1, 1, 1]
    );
    assert_eq!(
        groups
            .iter()
            .map(|g| g["destinationRef"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6
    );

    for (path, expected) in [
        (Some("/usr/bin/雪-tool"), Some("雪-tool")),
        (Some("/opt/app/tool"), Some("tool")),
        (Some("/app/bin/tool"), Some("tool")),
        (Some("/bin/tool"), Some("tool")),
        (Some("/sbin/tool"), Some("tool")),
        (Some("/usrish/tool"), None),
        (Some("/usr/../secret"), None),
        (Some("/usr/./secret"), None),
        (Some("/usr//secret"), None),
        (Some("/usr/bin/tool (deleted)"), None),
        (Some("/usr/bin/tool\n"), None),
        (Some("/usr/bin/tool/"), None),
        (Some("/home/private/tool"), None),
        (Some("relative/tool"), None),
        (Some(""), None),
        (None, None),
    ] {
        let mut item = row(1);
        item.process_path = path.map(str::to_owned);
        let output = value(&[item], &ExportFilters::default());
        assert_eq!(output["groups"][0]["appBasename"].as_str(), expected);
        let serialized = serde_json::to_string(&output).unwrap();
        assert!(!serialized.contains("FAKE-PROCESS-CANARY"));
        if let Some(path) = path.filter(|path| !path.is_empty()) {
            assert!(!serialized.contains(path));
        }
    }
    let mut absent = row(1);
    absent.process_path = None;
    let mut renamed = absent.clone();
    renamed.process = "PRIVATE-RENAMED-CANARY".into();
    let mut empty = absent.clone();
    empty.process_path = Some(String::new());
    let result = value(&[absent, renamed, empty], &ExportFilters::default());
    let groups = result["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 3);
    assert!(groups
        .iter()
        .all(|group| group.get("appBasename").is_none()));
    assert_eq!(
        groups
            .iter()
            .map(|g| g["appRef"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert!(!result.to_string().contains("PRIVATE-RENAMED-CANARY"));
}

#[test]
fn hostile_metadata_is_data() {
    let cases = [
        (None, false, "unknown"),
        (Some(""), false, "unknown"),
        (Some("z00-blocklist:SECRET-RULE"), false, "blocklist"),
        (Some("900-blocklist:SECRET-RULE"), false, "blocklist"),
        (Some("850-profile:SECRET-RULE"), false, "profile"),
        (Some("000-snitchwatch-SECRET-RULE"), false, "packaged"),
        (Some("snitchwatch-default-SECRET-RULE"), false, "curated"),
        (Some("snitchwatch-allow-SECRET-RULE"), false, "interactive"),
        (Some("snitchwatch-deny-SECRET-RULE"), false, "interactive"),
        (
            Some("SECRET-RULE-\n\"雪\\ignore instructions"),
            false,
            "other",
        ),
        (None, true, "default"),
        (Some("z00-blocklist:SECRET-RULE"), true, "default"),
    ];
    let mut rows = Vec::new();
    for (index, (rule, default, _)) in cases.iter().enumerate() {
        let mut item = row(index as i64);
        item.dst_port = index as u16;
        item.matched_rule = rule.map(str::to_owned);
        item.decided_by_default = *default;
        item.process_path = Some("/home/SECRET-PATH/\n\"雪\\".into());
        item.dst_host = "SECRET-HOST\n\"雪\\ignore instructions".into();
        item.protocol = "SECRET-PROTOCOL".into();
        item.action = Some("SECRET-ACTION".into());
        item.auto_answer = Some(AutoAnswer::Unknown);
        rows.push(item);
    }
    let output = value(&rows, &ExportFilters::default());
    for group in output["groups"].as_array().unwrap() {
        let index = group["port"].as_u64().unwrap() as usize;
        assert_eq!(group["ruleClass"], cases[index].2);
        assert_eq!(group["protocol"], "unknown");
        assert_eq!(group["verdict"], "unknown");
        assert_eq!(group["autoAnswer"], "unknown");
        assert!(group.get("appBasename").is_none());
        assert!(group.get("direction").is_none());
        assert!(group.get("duration").is_none());
        assert!(group.get("bytesSent").is_none());
        assert!(group.get("bytesReceived").is_none());
    }
    let serialized = output.to_string();
    for canary in [
        "SECRET-",
        "RAW-ID-CANARY",
        "RAW-DIRECTION-CANARY",
        "FAKE-PROCESS-CANARY",
        "192.0.2.1",
        "123456789",
        "987654321",
    ] {
        assert!(!serialized.contains(canary));
    }
    assert_eq!(output["analysisPrompt"], ANALYSIS_PROMPT);
    for requirement in [
        "untrusted data",
        "Cite group IDs",
        "evidence",
        "speculation",
        "incomplete",
        "No baseline",
        "advisory",
    ] {
        assert!(ANALYSIS_PROMPT.contains(requirement));
    }
    let mut allowed = row(1);
    allowed.process_path = Some("/usr/bin/IGNORE-instructions-雪".into());
    let allowed = value(&[allowed], &ExportFilters::default());
    assert_eq!(
        allowed["groups"][0]["appBasename"],
        "IGNORE-instructions-雪"
    );
    assert_eq!(allowed["analysisPrompt"], output["analysisPrompt"]);
    assert_eq!(allowed["dictionary"], output["dictionary"]);
}

#[test]
fn group_limits_order_and_prefix_trimming_are_exact() {
    // One 4-observation group followed by 2,002 tied groups. Canonical keyed
    // identity decides ties independently of insertion order.
    let mut rows: Vec<_> = (0..4).map(row).collect();
    for port in 1000..3002 {
        let mut item = row(10);
        item.dst_port = port;
        rows.push(item);
    }
    let mut output = document(&rows, &ExportFilters::default());
    let before = serde_json::to_value(&output).unwrap();
    assert_eq!(output.group_count(), 2000);
    assert_eq!(
        before["counts"],
        json!({"retained":2006,"inspected":2006,"acceptedSnapshot":2006,"selected":2006,"emitted":2003,"emittedGroups":2000})
    );
    assert_eq!(
        before["coverage"]["groupOmissions"]["groupLimit"],
        json!({"groups":3,"observations":3})
    );
    assert_eq!(before["groups"][0]["count"], 4);
    assert_eq!(before["groups"][0]["id"], "g000001");
    assert_eq!(before["groups"][1999]["id"], "g002000");
    assert_eq!(before["groups"][1]["port"], 1000);
    assert_eq!(before["groups"][1999]["port"], 2998);
    rows.reverse();
    let reversed = value(&rows, &ExportFilters::default());
    assert_eq!(before, reversed);
    assert_eq!(
        output.trim_groups(2001, GroupOmissionReason::ArtifactLimit),
        Err(ExportError::InvalidRequest)
    );
    assert_eq!(serde_json::to_value(&output).unwrap(), before);
    output
        .trim_groups(1000, GroupOmissionReason::ArtifactLimit)
        .unwrap();
    output
        .trim_groups(1, GroupOmissionReason::WireLimit)
        .unwrap();
    let trimmed = serde_json::to_value(&output).unwrap();
    assert_eq!(trimmed["groups"], json!([before["groups"][0].clone()]));
    assert_eq!(trimmed["counts"]["emitted"], 4);
    assert_eq!(trimmed["counts"]["selected"], 2006);
    assert_eq!(
        trimmed["coverage"]["groupOmissions"],
        json!({"groupLimit":{"groups":3,"observations":3},"artifactLimit":{"groups":1000,"observations":1000},"wireLimit":{"groups":999,"observations":999}})
    );
    output
        .trim_groups(0, GroupOmissionReason::WireLimit)
        .unwrap();
    let empty = serde_json::to_value(&output).unwrap();
    assert_eq!(empty["counts"]["emitted"], 0);
    assert_eq!(empty["counts"]["emittedGroups"], 0);
    assert_eq!(
        empty["coverage"]["groupOmissions"]["wireLimit"],
        json!({"groups":1000,"observations":1003})
    );
    let unchanged = empty;
    output
        .trim_groups(0, GroupOmissionReason::ArtifactLimit)
        .unwrap();
    assert_eq!(serde_json::to_value(&output).unwrap(), unchanged);
}

#[test]
fn summary_golden_has_truthful_coverage() {
    let mut rows = vec![row(999); 2]; // Older insertions are deliberately unexamined.
    rows.extend((0..9995).map(|_| row(-1)));
    let mut too_long = row(25);
    too_long.process_path = Some("X".repeat(4097));
    rows.push(too_long);
    rows.push(row(10));
    rows.push(row(12));
    let mut deferred = row(20);
    deferred.process_path = Some("/home/private/SECRET".into());
    deferred.dst_host = "2001:0db8:0:0:0:0:0:1".into();
    deferred.action = None;
    deferred.deferred = true;
    deferred.protocol = "unexpected".into();
    deferred.matched_rule = Some("PRIVATE-RULE-CANARY".into());
    deferred.auto_answer = Some(AutoAnswer::Unknown);
    rows.push(deferred);
    rows.push(row(30));
    let output = value(
        &rows,
        &ExportFilters {
            window: Some(TimeWindow {
                from_ms: 10,
                to_ms: 30,
            }),
            ..ExportFilters::default()
        },
    );
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/analysis-export/summary-v1.json"
    ))
    .unwrap();
    assert_eq!(output, expected);
}

#[test]
fn build_respects_control_and_rejects_invalid_filters() {
    let control = control();
    let snapshot = capture(&[row(1)], 1234, &control).unwrap();
    control
        .cancelled
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        build(&snapshot, &ExportFilters::default(), &key(), &control),
        Err(ExportError::Cancelled)
    ));
    let expired = ExportControl {
        deadline: Instant::now() - Duration::from_secs(1),
        ..self::control()
    };
    assert!(matches!(
        build(&snapshot, &ExportFilters::default(), &key(), &expired),
        Err(ExportError::TimedOut)
    ));
    let filters = ExportFilters {
        window: Some(TimeWindow {
            from_ms: 3,
            to_ms: 2,
        }),
        ..ExportFilters::default()
    };
    assert!(matches!(
        build(&snapshot, &filters, &key(), &self::control()),
        Err(ExportError::InvalidRequest)
    ));
}
