//! What GUIs are told about each curated default.

use serde::{Deserialize, Serialize};

use super::reconcile::EntryStatus;

/// One entry, as the GUI lists it. Every text is the bridge's own: the
/// reviewed data file's, or fixed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CuratedDefaultSummary {
    pub id: String,
    /// The id of the group ([`super::CuratedGroup`]) it is offered in; empty
    /// from an older bridge, which had no groups.
    #[serde(default)]
    pub group: String,
    /// The program's exact path.
    pub program: String,
    /// Exactly what the rule allows, in plain text.
    pub allows: String,
    /// Why it is offered.
    pub why: String,
    /// On: the user turned it on, or, not chosen yet, its rule is already
    /// in the firewall and enabled. Off otherwise.
    pub on: bool,
    /// Allows more than one named place (any address): a GUI never turns it
    /// on in bulk ("Turn all on"), only by itself. Omitted when false, so an
    /// older bridge never sends it and an older GUI ignores it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub broad: bool,
    pub status: EntryStatus,
    /// Why the last command for it failed, in fixed text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// A rule under the curated prefix that no entry offers and Snitchwatch
/// recorded no copy of (#113, `super::orphans`): listed under "No longer
/// recommended" with Remove. Only the bridge's own facts: the id from its
/// reserved name, whether it is enabled, and fixed text; never the rule's
/// daemon-sourced content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CuratedOrphanSummary {
    /// The id under the prefix (`snitchwatch-default-<id>`).
    pub id: String,
    /// Whether the daemon's rule is enabled (it may be turned off on the
    /// Rules page).
    pub enabled: bool,
    /// The daemon refused its delete after dropping it from memory: its file
    /// may remain and bring it back when the service restarts. Not
    /// removable until then. Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub file_left: bool,
    /// Why the last removal failed, in fixed text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(broad: bool) -> CuratedDefaultSummary {
        CuratedDefaultSummary {
            id: "x".into(),
            group: "g".into(),
            program: "/usr/bin/x".into(),
            allows: "a".into(),
            why: "w".into(),
            on: false,
            broad,
            status: EntryStatus::Off,
            problem: None,
        }
    }

    #[test]
    fn broad_is_sent_only_when_true_and_round_trips() {
        let broad = serde_json::to_value(summary(true)).unwrap();
        assert_eq!(broad["broad"], true);
        let back: CuratedDefaultSummary = serde_json::from_value(broad).unwrap();
        assert_eq!(back, summary(true));
        let plain = serde_json::to_value(summary(false)).unwrap();
        assert!(plain.get("broad").is_none(), "{plain}");
        assert_eq!(
            serde_json::from_value::<CuratedDefaultSummary>(plain).unwrap(),
            summary(false)
        );
    }

    /// An older bridge's summary has no `group`: it reads as ungrouped.
    #[test]
    fn a_summary_without_a_group_parses_as_ungrouped_and_a_group_round_trips() {
        let old = serde_json::json!({
            "id": "flatpak-flathub", "program": "/usr/bin/flatpak", "allows": "a",
            "why": "w", "on": true, "status": "installed"
        });
        assert_eq!(
            serde_json::from_value::<CuratedDefaultSummary>(old)
                .unwrap()
                .group,
            ""
        );
        let wire = serde_json::to_value(summary(false)).unwrap();
        assert_eq!(wire["group"], "g");
        assert_eq!(
            serde_json::from_value::<CuratedDefaultSummary>(wire).unwrap(),
            summary(false)
        );
    }

    /// An orphan summary leaves out `fileLeft` and `problem` when unset.
    #[test]
    fn an_orphan_summary_round_trips_and_leaves_out_its_unset_fields() {
        let plain = CuratedOrphanSummary {
            id: "retired".into(),
            enabled: false,
            file_left: false,
            problem: None,
        };
        let wire = serde_json::to_value(&plain).unwrap();
        assert_eq!(wire, serde_json::json!({"id": "retired", "enabled": false}));
        assert_eq!(
            serde_json::from_value::<CuratedOrphanSummary>(wire).unwrap(),
            plain
        );
        let left = CuratedOrphanSummary {
            file_left: true,
            problem: Some("p".into()),
            ..plain
        };
        let wire = serde_json::to_value(&left).unwrap();
        assert_eq!(wire["fileLeft"], true);
        assert_eq!(wire["problem"], "p");
        assert_eq!(
            serde_json::from_value::<CuratedOrphanSummary>(wire).unwrap(),
            left
        );
    }

    /// An older bridge's summary has no `broad`: it reads as not broad.
    #[test]
    fn an_older_summary_without_the_field_still_parses() {
        let old = serde_json::json!({
            "id": "flatpak-flathub",
            "program": "/usr/bin/flatpak",
            "allows": "a",
            "why": "w",
            "on": true,
            "status": "installed"
        });
        let parsed: CuratedDefaultSummary = serde_json::from_value(old).unwrap();
        assert!(!parsed.broad && parsed.on);
    }
}
