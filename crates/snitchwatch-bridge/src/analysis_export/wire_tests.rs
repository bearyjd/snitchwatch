use super::*;
use crate::ws_messages::{decode_client_message, ClientMessage, ServerMessage};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn control() -> ExportControl {
    ExportControl {
        deadline: Instant::now() + Duration::from_secs(30),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}
fn empty_document() -> ExportDocument {
    let c = control();
    build(
        &capture(&[], 100, &c).unwrap(),
        &ExportFilters::default(),
        &fresh_key().unwrap(),
        &c,
    )
    .unwrap()
}
fn groups_document(count: usize, basename: &str) -> ExportDocument {
    let mut doc = empty_document();
    doc.groups = (0..count)
        .map(|i| dto::ExportGroup {
            id: format!("g{:06}", i + 1),
            app_ref: format!("{:032x}", i),
            app_basename: Some(basename.into()),
            destination_ref: "d".repeat(32),
            port: 443,
            protocol: ExportProtocol::Tcp,
            verdict: ExportVerdict::Allow,
            rule_class: ExportRuleClass::Unknown,
            auto_answer: ExportAutoAnswer::None,
            count: count - i,
            first_observed_at_ms: 1,
            last_observed_at_ms: 2,
        })
        .collect();
    doc.counts.selected = count * (count + 1) / 2;
    doc.counts.emitted = doc.counts.selected;
    doc.counts.emitted_groups = count;
    doc
}
fn request() -> String {
    r#"{"action":"requestCacheSummaryExport","requestId":"request-1","schemaVersion":1,"filters":{}}"#.into()
}
#[test]
fn strict_export_request_validation() {
    let raw = request();
    let parsed = decode_client_message(&raw).unwrap();
    assert!(matches!(
        parsed,
        ClientMessage::RequestCacheSummaryExport {
            reply: None,
            session: None,
            ..
        }
    ));
    assert!(serde_json::from_str::<ClientMessage>(&raw).is_ok());
    let at_limit = format!("{}{}", raw, " ".repeat(8192 - raw.len()));
    assert!(decode_client_message(&at_limit).is_ok());
    assert_eq!(
        decode_client_message(&(at_limit + " ")).unwrap_err().export,
        Some(ExportError::InvalidRequest)
    );
    for bad in [
        raw.replace("\"schemaVersion\":1", "\"schemaVersion\":2"),
        raw.replace("request-1", "bad id"),
        raw.replace("request-1", &"a".repeat(65)),
        raw.replace("\"filters\":{}", "\"filters\":{\"unknown\":true}"),
        raw.replace(
            "\"filters\":{}",
            "\"filters\":{\"verdicts\":[\"allow\",\"allow\"]}",
        ),
        raw.replace(
            "\"filters\":{}",
            "\"filters\":{\"window\":{\"fromMs\":0,\"toMs\":0}}",
        ),
        raw.replace(
            "\"filters\":{}",
            "\"filters\":{\"window\":{\"fromMs\":0,\"toMs\":1.5}}",
        ),
        raw.replace(
            "\"filters\":{}",
            "\"filters\":{\"window\":{\"fromMs\":0,\"fromMs\":1,\"toMs\":2}}",
        ),
        raw.replace(
            "\"filters\":{}",
            "\"filters\":{\"app\":{\"kind\":\"path\",\"value\":\"a\",\"value\":\"b\"}}",
        ),
        raw.replace(
            "\"filters\":{}",
            "\"filters\":{\"app\":{\"kind\":\"missingPath\",\"extra\":0}}",
        ),
        raw.replace("\"filters\":{}", "\"filters\":{},\"session\":{}"),
        raw.replace("\"filters\":{}", "\"filters\":{},\"reply\":{}"),
        raw.replace("\"filters\":{}", "\"filters\":{},\"filters\":{}"),
        raw.replace(
            "\"requestId\":\"request-1\"",
            "\"requestId\":\"request-1\",\"requestId\":\"request-2\"",
        ),
    ] {
        assert!(
            decode_client_message(&bad).is_err(),
            "accepted invalid request"
        );
        assert!(
            serde_json::from_str::<ClientMessage>(&bad).is_err(),
            "generic serde accepted invalid request"
        );
    }
    for length in [4096, 4097] {
        let raw = json!({"action":"requestCacheSummaryExport","requestId":"r","schemaVersion":1,
            "filters":{"app":{"kind":"path","value":"x".repeat(length)}}})
        .to_string();
        assert_eq!(decode_client_message(&raw).is_ok(), length == 4096);
    }
    let e = decode_client_message(&raw.replace("\"schemaVersion\":1", "\"schemaVersion\":2"))
        .unwrap_err();
    assert_eq!(e.export, Some(ExportError::UnsupportedVersion));
    let bad = raw.replace(
        "\"requestId\":\"request-1\"",
        "\"requestId\":\"request-1\",\"requestId\":\"request-2\"",
    );
    assert_eq!(decode_client_message(&bad).unwrap_err().request_id, None);
    assert!(decode_client_message(
        r#"{"action":"cancelCacheSummaryExport","requestId":"r","schemaVersion":1}"#
    )
    .is_ok());
    assert!(decode_client_message(
        r#"{"action":"cancelCacheSummaryExport","requestId":"r","schemaVersion":2}"#
    )
    .is_err());
    assert!(decode_client_message(r#"{"action":"exportRules","requestId":"legacy"}"#).is_ok());
}
#[test]
fn escaped_reply_caps_drop_whole_groups() {
    let prepared = prepare_reply(
        "r",
        groups_document(2000, &"\"\\\n\u{0001}".repeat(150)),
        &control(),
    )
    .unwrap();
    assert!(prepared.artifact.len() <= 1_048_576);
    assert!(prepared.wire.len() <= 1_052_658);
    let wire: ServerMessage = serde_json::from_str(&prepared.wire).unwrap();
    let ServerMessage::CacheSummaryExport { artifact, .. } = wire else {
        panic!("success envelope required")
    };
    assert_eq!(artifact.as_bytes(), prepared.artifact.as_bytes());
    let value: Value = serde_json::from_str(&artifact).unwrap();
    let groups = value["groups"].as_array().unwrap();
    assert!(groups.len() < 2000 && !groups.is_empty());
    assert_eq!(value["counts"]["emittedGroups"], groups.len());
    let omissions = &value["coverage"]["groupOmissions"];
    assert_eq!(
        groups.len()
            + omissions["artifactLimit"]["groups"].as_u64().unwrap() as usize
            + omissions["wireLimit"]["groups"].as_u64().unwrap() as usize,
        2000
    );
    let omitted = omissions["artifactLimit"]["observations"].as_u64().unwrap()
        + omissions["wireLimit"]["observations"].as_u64().unwrap();
    assert_eq!(
        value["counts"]["emitted"].as_u64().unwrap() + omitted,
        2_001_000
    );
}
#[test]
fn reply_serialization_probe_count_is_bounded() {
    let mut attempts = Vec::new();
    let mut expected_artifact = (0usize, 0usize);
    let mut expected_wire = (0usize, 0usize);
    let result =
        session::prepare_reply_with_probe("r", groups_document(2000, "x"), &control(), |doc| {
            let count = doc.group_count();
            attempts.push(count);
            let omitted = &doc.coverage.group_omissions;
            assert_eq!(
                (
                    omitted.artifact_limit.groups,
                    omitted.artifact_limit.observations
                ),
                expected_artifact
            );
            assert_eq!(
                (omitted.wire_limit.groups, omitted.wire_limit.observations),
                expected_wire
            );
            assert_eq!(
                doc.counts.emitted + expected_artifact.1 + expected_wire.1,
                2_001_000
            );
            for (index, group) in doc.groups.iter().enumerate() {
                assert_eq!(group.id, format!("g{:06}", index + 1));
                assert_eq!(group.count, 2000 - index);
            }
            if count > 0 {
                let keep = count / 2;
                let observations = doc.groups[keep..]
                    .iter()
                    .map(|group| group.count)
                    .sum::<usize>();
                let (reason, expected) = if attempts.len() % 2 == 1 {
                    (GroupOmissionReason::ArtifactLimit, &mut expected_artifact)
                } else {
                    (GroupOmissionReason::WireLimit, &mut expected_wire)
                };
                expected.0 += count - keep;
                expected.1 += observations;
                Err(reason)
            } else {
                Ok(("{}".into(), "{}".into()))
            }
        })
        .unwrap();
    assert_eq!(
        attempts,
        [2000, 1000, 500, 250, 125, 62, 31, 15, 7, 3, 1, 0]
    );
    assert_eq!(&*result.artifact, "{}");
    let c = control();
    let mut calls = 0;
    let result = session::prepare_reply_with_probe("r", groups_document(2000, "x"), &c, |_| {
        calls += 1;
        c.cancelled.store(true, Ordering::Release);
        Err(GroupOmissionReason::ArtifactLimit)
    });
    assert!(matches!(result, Err(ExportError::Cancelled)));
    assert_eq!(calls, 1);
    let mut doc = empty_document();
    doc.analysis_prompt = Box::leak("x".repeat(1_048_577).into_boxed_str());
    assert!(matches!(
        prepare_reply("r", doc, &control()),
        Err(ExportError::TooLarge)
    ));
}

#[test]
fn wire_omission_uses_actual_full_envelope_and_static_debug() {
    let prepared = prepare_reply(
        "private-id-canary",
        groups_document(1000, &"\"".repeat(300)),
        &control(),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&prepared.artifact).unwrap();
    assert_eq!(
        value["coverage"]["groupOmissions"]["artifactLimit"]["groups"],
        0
    );
    assert_eq!(
        value["coverage"]["groupOmissions"]["wireLimit"]["groups"],
        500
    );
    assert_eq!(value["counts"]["emittedGroups"], 500);
    assert!(prepared.wire.len() <= 1_052_658);
    let message: ServerMessage = serde_json::from_str(&prepared.wire).unwrap();
    assert_eq!(format!("{message:?}"), "ServerMessage(CacheSummaryExport)");
    let raw = request().replace(
        "\"filters\":{}",
        "\"filters\":{\"app\":{\"kind\":\"path\",\"value\":\"private-selector-canary\"}}",
    );
    let message = decode_client_message(&raw).unwrap();
    assert_eq!(
        format!("{message:?}"),
        "ClientMessage(RequestCacheSummaryExport)"
    );
    let wire = serde_json::to_value(message).unwrap();
    assert!(wire.get("session").is_none() && wire.get("reply").is_none());
}
#[tokio::test]
async fn export_without_private_context_has_no_upstream_effect_or_artifact_queue() {
    use crate::cache::connections::ConnectionCache;
    use crate::translator::upstream::{apply, UpstreamEffect};
    use crate::ws_messages::ReplyTo;
    let mut cache = ConnectionCache::new(100);
    for raw in [
        request(),
        r#"{"action":"cancelCacheSummaryExport","requestId":"r","schemaVersion":1}"#.into(),
    ] {
        assert_eq!(
            apply(&mut cache, decode_client_message(&raw).unwrap()).unwrap(),
            UpstreamEffect::None
        );
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let reply = ReplyTo::new(tx);
    let success = || ServerMessage::CacheSummaryExport {
        request_id: "r".into(),
        artifact: "private-artifact-canary".into(),
    };
    assert!(!reply.try_send(success()));
    assert!(!reply.send(success()).await);
    assert!(rx.try_recv().is_err());
}

#[test]
fn export_decode_failures_do_not_debug_request_identifiers() {
    let error = decode_client_message(
        &request()
            .replace("request-1", "private-request-canary")
            .replace("\"schemaVersion\":1", "\"schemaVersion\":2"),
    )
    .unwrap_err();
    assert_eq!(error.request_id.as_deref(), Some("private-request-canary"));
    assert!(!format!("{error:?}").contains("private-request-canary"));
}

#[test]
fn strict_export_nested_duplicates_unknown_options_and_exact_integers() {
    let raw = request();
    for filters in [
        r#"{"window":{"fromMs":0,"toMs":2,"toMs":3}}"#,
        r#"{"window":{"fromMs":0.0,"toMs":2}}"#,
        r#"{"window":{"fromMs":-1,"toMs":2}}"#,
        r#"{"window":{"fromMs":0,"toMs":253402300800000}}"#,
        r#"{"app":{"kind":"missingPath","kind":"path","value":"a"}}"#,
        r#"{"app":{"kind":"path","value":"a","extra":0}}"#,
        r#"{"app":{"kind":"missingPath"},"app":{"kind":"missingPath"}}"#,
        r#"{"verdicts":[],"verdicts":["allow"]}"#,
        r#"{"verdicts":["allow","deny","blocklist","pending","unknown","allow"]}"#,
        r#"{"verdicts":["forged"]}"#,
        r#"{"mode":"raw"}"#,
    ] {
        let bad = raw.replace("\"filters\":{}", &format!("\"filters\":{filters}"));
        assert_eq!(
            decode_client_message(&bad).unwrap_err().export,
            Some(ExportError::InvalidRequest)
        );
        assert!(serde_json::from_str::<ClientMessage>(&bad).is_err());
    }
    for id in ["", "not_valid", "with space", "\u{00e9}"] {
        let bad = raw.replace("request-1", id);
        assert_eq!(decode_client_message(&bad).unwrap_err().request_id, None);
    }
    for id in ["a".into(), "a".repeat(64)] {
        assert!(decode_client_message(&raw.replace("request-1", &id)).is_ok());
    }
    let duplicate_action = raw.replace(
        "\"action\":\"requestCacheSummaryExport\"",
        "\"action\":\"requestCacheSummaryExport\",\"action\":\"undo\"",
    );
    let error = decode_client_message(&duplicate_action).unwrap_err();
    assert_eq!(error.export, Some(ExportError::InvalidRequest));
    assert_eq!(error.request_id, None);
    let oversize_late_action = format!(
        r#"{{"filters":{{"app":{{"kind":"path","value":"{}"}}}},"requestId":"r","schemaVersion":1,"action":"requestCacheSummaryExport"}}"#,
        "x".repeat(8193)
    );
    assert_eq!(
        decode_client_message(&oversize_late_action)
            .unwrap_err()
            .export,
        Some(ExportError::InvalidRequest)
    );
    let mut doc = empty_document();
    // The artifact itself fits; escaping the empty envelope does not.
    doc.analysis_prompt = Box::leak("\"".repeat(300_000).into_boxed_str());
    assert!(matches!(
        prepare_reply("r", doc, &control()),
        Err(ExportError::TooLarge)
    ));
}
