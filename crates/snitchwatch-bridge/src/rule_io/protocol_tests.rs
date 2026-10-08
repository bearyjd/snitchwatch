//! The import/export messages' wire shape (additive `ws_messages` variants).

use super::*;
use crate::ws_messages::{ClientMessage, ServerMessage};
use serde_json::json;

#[test]
fn client_messages_use_camel_case_actions_and_fields() {
    let export: ClientMessage = serde_json::from_value(json!({ "action": "exportRules" })).unwrap();
    assert_eq!(export, ClientMessage::ExportRules);

    let preview: ClientMessage = serde_json::from_value(json!({
        "action": "previewRulesImport",
        "document": { "format": "snitchwatch.rules" },
    }))
    .unwrap();
    assert!(matches!(preview, ClientMessage::PreviewRulesImport { .. }));

    let apply: ClientMessage = serde_json::from_value(json!({
        "action": "applyRulesImport", "previewId": "p", "include": ["a", "b"],
    }))
    .unwrap();
    assert_eq!(
        apply,
        ClientMessage::ApplyRulesImport {
            preview_id: "p".into(),
            include: vec!["a".into(), "b".into()],
        }
    );
}

#[test]
fn server_messages_round_trip() {
    let messages = vec![
        ServerMessage::RulesExport {
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
            reason: ExportUnavailable::REASON.into(),
        },
        ServerMessage::RulesImportPreview {
            preview_id: "p".into(),
            items: Vec::new(),
        },
        ServerMessage::RulesImportRefused {
            reason: DocumentError::Newer.describe().into(),
        },
        ServerMessage::RulesImportProgress {
            name: "a".into(),
            outcome: ImportOutcome::Rejected {
                reason: "bad regexp".into(),
            },
        },
        ServerMessage::RulesImportResult {
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
        applied: 1,
        rejected: 0,
        not_sent: 0,
        no_answer: 0,
    })
    .unwrap();
    assert_eq!(result["action"], "rulesImportResult");
    assert_eq!(result["notSent"], 0);
    let progress = serde_json::to_value(ImportOutcome::NotSent {
        reason: "busy".into(),
    })
    .unwrap();
    assert_eq!(progress, json!({ "status": "notSent", "reason": "busy" }));
}
