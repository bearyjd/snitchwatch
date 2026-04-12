//! Rule classification helpers for blocklist reconciliation.
//!
//! The materializer writes a JSON tag into each blocklist rule's `description`
//! field: `{"snitchwatch":{"source":"blocklist","list_id":"<id>","entry":"<host>"}}`.
//! `classify_rule` reads that tag and returns a `RuleCategory` so the
//! reconciler can distinguish user-authored rules from bridge-managed ones.

/// Classification of an opensnitchd rule for reconciliation purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleCategory {
    /// A user-authored rule — must never be touched by the bridge.
    User,
    /// A bridge-managed blocklist rule for the given list.
    Blocklist { list_id: String },
}

/// Classify a rule by inspecting its `description` JSON tag.
///
/// Returns `RuleCategory::Blocklist` when the description contains a valid
/// snitchwatch blocklist tag, and `RuleCategory::User` for everything else
/// (missing tag, malformed JSON, wrong source value, missing list_id).
pub fn classify_rule(_name: &str, description: &str) -> RuleCategory {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(description) {
        if let Some(meta) = parsed.get("snitchwatch") {
            if meta.get("source").and_then(|v| v.as_str()) == Some("blocklist") {
                if let Some(list_id) = meta.get("list_id").and_then(|v| v.as_str()) {
                    return RuleCategory::Blocklist {
                        list_id: list_id.to_string(),
                    };
                }
            }
        }
    }
    RuleCategory::User
}

#[cfg(test)]
mod blocklist_reconciliation_tests {
    use super::*;

    #[test]
    fn classify_900_band_rule_as_blocklist() {
        let cat = classify_rule(
            "900-blocklist:stevenblack:0001-doubleclick.net",
            r#"{"snitchwatch":{"source":"blocklist","list_id":"stevenblack","entry":"doubleclick.net"}}"#,
        );
        assert_eq!(
            cat,
            RuleCategory::Blocklist {
                list_id: "stevenblack".into(),
            }
        );
    }

    #[test]
    fn classify_user_rule_as_user() {
        let cat = classify_rule("050-allow-firefox", "");
        assert_eq!(cat, RuleCategory::User);
    }

    #[test]
    fn classify_legacy_900_rule_without_tag_as_user() {
        let cat = classify_rule("900-legacy-rule", "{}");
        assert_eq!(cat, RuleCategory::User);
    }
}
