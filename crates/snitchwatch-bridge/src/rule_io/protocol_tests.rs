//! The import/export messages' wire shape (additive `ws_messages` variants):
//! requests carry a client request id that every answer echoes, and an
//! apply's progress and result carry its preview id (P2.7 review #8).

use super::*;
use crate::ws_messages::{ClientMessage, ServerMessage};
use serde_json::json;

#[test]
fn client_messages_use_camel_case_actions_and_fields() {
    let export: ClientMessage =
        serde_json::from_value(json!({ "action": "exportRules", "requestId": "r1" })).unwrap();
    assert!(
        matches!(export, ClientMessage::ExportRules { ref request_id, .. } if request_id == "r1")
    );

    let preview: ClientMessage = serde_json::from_value(json!({
        "action": "previewRulesImport",
        "requestId": "r2",
        "document": { "format": "snitchwatch.rules" },
    }))
    .unwrap();
    assert!(
        matches!(preview, ClientMessage::PreviewRulesImport { ref request_id, .. } if request_id == "r2")
    );

    let apply: ClientMessage = serde_json::from_value(json!({
        "action": "applyRulesImport", "requestId": "r3", "previewId": "p", "include": ["a", "b"],
    }))
    .unwrap();
    match apply {
        ClientMessage::ApplyRulesImport {
            request_id,
            preview_id,
            include,
            reply,
        } => {
            assert_eq!((request_id.as_str(), preview_id.as_str()), ("r3", "p"));
            assert_eq!(include, vec!["a", "b"]);
            assert!(reply.is_none(), "a reply channel never comes from the wire");
        }
        other => panic!("{other:?}"),
    }
    // A reply channel is never serialized either.
    let text = serde_json::to_string(&ClientMessage::ExportRules {
        request_id: "r".into(),
        reply: None,
    })
    .unwrap();
    assert_eq!(text, r#"{"action":"exportRules","requestId":"r"}"#);
}

#[test]
fn server_messages_round_trip_and_echo_their_ids() {
    let messages = vec![
        ServerMessage::RulesExport {
            request_id: "r1".into(),
            document: Document {
                format: FORMAT.into(),
                version: VERSION,
                exported_at_unix_ms: 7,
                source: Source::default(),
                rules: vec![json!({ "name": "a" })],
            },
            omitted: OmittedCounts {
                once: 1,
                ..Default::default()
            },
        },
        ServerMessage::RulesExportUnavailable {
            request_id: "r1".into(),
            reason: ExportUnavailable::REASON.into(),
        },
        ServerMessage::RulesImportPreview {
            request_id: "r2".into(),
            preview_id: "p".into(),
            items: Vec::new(),
        },
        ServerMessage::RulesImportRefused {
            request_id: "r2".into(),
            reason: DocumentError::Newer.describe().into(),
        },
        ServerMessage::RulesImportProgress {
            preview_id: "p".into(),
            name: "a".into(),
            outcome: ImportOutcome::Rejected {
                reason: "bad regexp".into(),
            },
        },
        ServerMessage::RulesImportResult {
            preview_id: "p".into(),
            applied: 1,
            rejected: 2,
            not_sent: 3,
            no_answer: 4,
        },
    ];
    for message in messages {
        let text = serde_json::to_string(&message).unwrap();
        let back: ServerMessage = serde_json::from_str(&text).unwrap();
        assert_eq!(back, message, "{text}");
    }
    let result = serde_json::to_value(ServerMessage::RulesImportResult {
        preview_id: "p".into(),
        applied: 1,
        rejected: 0,
        not_sent: 0,
        no_answer: 0,
    })
    .unwrap();
    assert_eq!(result["action"], "rulesImportResult");
    assert_eq!(result["previewId"], "p");
    assert_eq!(result["notSent"], 0);
    let progress = serde_json::to_value(ImportOutcome::NotSent {
        reason: "busy".into(),
    })
    .unwrap();
    assert_eq!(progress, json!({ "status": "notSent", "reason": "busy" }));
}
