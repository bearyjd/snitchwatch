//! "No longer recommended" on the Recommended page (#113): rules under the
//! curated prefix that the bridge says no entry offers and Snitchwatch
//! recorded no copy of. Qt-free; `CuratedStore` keeps the bridge's list and
//! `CuratedDefaultsModel` hands it to QML as JSON.
//!
//! Every text is fixed here, except the bridge's fixed problem text; the
//! only daemon-derived text is the rule's name under the reserved prefix.
//! Remove is offered only for a listed orphan, on a bridge that changes
//! recommended rules; the bridge checks again before it deletes anything.

use serde_json::{json, Value};
use snitchwatch_bridge::curated::wire::CuratedOrphanSummary;
use snitchwatch_bridge::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;
use snitchwatch_bridge::ws_messages::ClientMessage;

/// Where an orphan stands, in one fixed sentence.
pub fn status_text(orphan: &CuratedOrphanSummary) -> &'static str {
    match (orphan.file_left, orphan.enabled) {
        (true, _) => "Removed from the firewall, but its saved file may remain.",
        (false, true) => "In the firewall, but no longer in Snitchwatch's recommended rules.",
        (false, false) => {
            "In the firewall, turned off on the Rules page, and no longer in Snitchwatch's \
             recommended rules."
        }
    }
}

/// Whether `orphan` offers Remove: listed (not already out of the daemon's
/// memory) on a bridge that changes recommended rules.
pub fn can_remove(orphan: &CuratedOrphanSummary, usable: bool) -> bool {
    usable && !orphan.file_left
}

/// The orphans as JSON for QML.
pub fn view_json(orphans: &[CuratedOrphanSummary], usable: bool) -> String {
    let rows: Vec<Value> = orphans
        .iter()
        .map(|o| {
            json!({
                "orphanId": o.id,
                "ruleName": format!("{CURATED_DEFAULT_RULE_NAME_PREFIX}{}", o.id),
                "statusText": status_text(o),
                "problem": o.problem.as_deref().unwrap_or_default(),
                "canRemove": can_remove(o, usable),
            })
        })
        .collect();
    Value::Array(rows).to_string()
}

/// The request removing the orphan `id`, once the user confirmed; `None`
/// unless it is listed and offers Remove.
pub fn removal(orphans: &[CuratedOrphanSummary], usable: bool, id: &str) -> Option<ClientMessage> {
    orphans
        .iter()
        .any(|o| o.id == id && can_remove(o, usable))
        .then(|| ClientMessage::RemoveCuratedOrphan { id: id.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orphan(id: &str, enabled: bool, file_left: bool) -> CuratedOrphanSummary {
        CuratedOrphanSummary {
            id: id.into(),
            enabled,
            file_left,
            problem: None,
        }
    }

    /// The store takes the bridge's list, and a new bridge session starts
    /// without one; a bridge that changes none offers no Remove.
    #[test]
    fn the_store_follows_the_bridges_list_per_session() {
        use crate::curated_defaults::CuratedStore;
        use snitchwatch_bridge::ws_messages::{ServerMessage, StorageStatus};
        let message = |unavailable: Option<&str>| ServerMessage::SetCuratedDefaults {
            entries: Vec::new(),
            groups: Vec::new(),
            orphans: vec![orphan("old", true, false)],
            storage: StorageStatus {
                persistent: true,
                reason: None,
                unreadable: false,
            },
            unavailable: unavailable.map(str::to_string),
        };
        let mut store = CuratedStore::default();
        assert_eq!(store.orphans_json(), "[]");
        assert!(store.apply(1, &message(None)));
        assert!(store.orphan_removal("old").is_some());
        assert!(store.orphans_json().contains("snitchwatch-default-old"));
        store.apply(1, &message(Some("per-user")));
        assert_eq!(store.orphan_removal("old"), None);
        store.apply(
            2,
            &ServerMessage::ProfileChanged {
                active_profile_id: None,
            },
        );
        assert_eq!(
            store.orphans_json(),
            "[]",
            "an earlier session's list stays"
        );
    }

    #[test]
    fn only_a_listed_removable_orphan_is_asked_for() {
        let list = [orphan("old", true, false), orphan("gone", true, true)];
        assert_eq!(
            removal(&list, true, "old"),
            Some(ClientMessage::RemoveCuratedOrphan { id: "old".into() })
        );
        assert_eq!(removal(&list, true, "gone"), None, "file left");
        assert_eq!(removal(&list, true, "other"), None, "not listed");
        assert_eq!(removal(&list, false, "old"), None, "bridge changes none");
    }

    #[test]
    fn each_row_says_where_it_stands_in_fixed_words() {
        let mut failed = orphan("off", false, false);
        failed.problem = Some("The firewall service didn't answer.".into());
        let list = [
            orphan("old", true, false),
            failed,
            orphan("gone", true, true),
        ];
        let rows: Vec<Value> = serde_json::from_str(&view_json(&list, true)).unwrap();
        assert_eq!(rows[0]["ruleName"], "snitchwatch-default-old");
        assert_eq!(rows[0]["canRemove"], true);
        assert_eq!(rows[0]["problem"], "");
        assert!(rows[1]["statusText"]
            .as_str()
            .unwrap()
            .contains("turned off on the Rules page"));
        assert_eq!(rows[1]["problem"], "The firewall service didn't answer.");
        assert_eq!(rows[2]["canRemove"], false);
        assert!(rows[2]["statusText"]
            .as_str()
            .unwrap()
            .contains("saved file may remain"));
        let unusable: Vec<Value> = serde_json::from_str(&view_json(&list, false)).unwrap();
        assert!(unusable.iter().all(|r| r["canRemove"] == false));
    }
}
