//! What GUIs are told about each curated default.

use serde::{Deserialize, Serialize};

use super::reconcile::EntryStatus;

/// One entry, as the GUI lists it. Every text is the bridge's own: the
/// reviewed data file's, or fixed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CuratedDefaultSummary {
    pub id: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(broad: bool) -> CuratedDefaultSummary {
        CuratedDefaultSummary {
            id: "x".into(),
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
