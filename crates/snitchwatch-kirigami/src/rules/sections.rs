//! Where a rule comes from, in words: the Rules list's section headings and
//! the inspector's Source (issue #102 follow-up, PR #106 review). The list
//! is in the order the firewall checks rules, so a source can come back
//! after another (user rules named after the `850-profile:` band follow the
//! profile rules); its later sections read "… (continued)".

use std::mem::discriminant;

use super::row_store::{RuleSource, RulesStore};

pub const USER_RULES: &str = "User rules";
pub const PROFILE_RULES: &str = "Profile rules";
pub const BLOCKLIST_RULES: &str = "Blocklist rules";

impl RuleSource {
    /// The source's name, as the inspector shows it.
    pub fn label(&self) -> &'static str {
        match self {
            RuleSource::User => USER_RULES,
            RuleSource::Profile => PROFILE_RULES,
            RuleSource::Blocklist { .. } => BLOCKLIST_RULES,
        }
    }
}

/// The heading of the section row `index` is in: its source's label, with
/// "(continued)" when that source already had a section above. Every
/// blocklist's rules are one source.
pub fn section_label(store: &RulesStore, index: usize) -> Option<String> {
    let rules = store.rules();
    let kind = discriminant(&rules.get(index)?.source());
    let same = |i: usize| discriminant(&rules[i].source()) == kind;
    let start = (0..index)
        .rev()
        .take_while(|&i| same(i))
        .last()
        .unwrap_or(index);
    let label = rules[index].source().label();
    Some(if (0..start).any(same) {
        format!("{label} (continued)")
    } else {
        label.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::row_store::{found_rule_json, Rule};
    use snitchwatch_bridge::ws_messages::ServerMessage;

    fn store(names: &[&str]) -> RulesStore {
        let mut store = RulesStore::new();
        store.apply(&ServerMessage::SetRules {
            rules: names
                .iter()
                .map(|name| {
                    serde_json::to_value(Rule {
                        name: name.to_string(),
                        enabled: true,
                        action: "allow".into(),
                        ..Default::default()
                    })
                    .unwrap()
                })
                .collect(),
        });
        store
    }

    /// A profile's rule is labelled as one, in the list and the inspector,
    /// rather than as the user's own.
    #[test]
    fn profile_band_rules_are_labelled_profile_rules() {
        let name = "850-profile:home:0001-allow-dns";
        let s = store(&[name, "849-mine"]);
        assert_eq!(s.find_by_name(name).unwrap().source(), RuleSource::Profile);
        assert_eq!(
            s.find_by_name("849-mine").unwrap().source(),
            RuleSource::User
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&found_rule_json(&s, name).unwrap()).unwrap();
        assert_eq!(parsed["source"], "profile");
        assert_eq!(parsed["sourceLabel"], PROFILE_RULES);
        assert_eq!(parsed["blocklistId"], "");
    }

    /// The list is in check order, so user rules can come back after the
    /// profile rules; blocklists of different ids are one section.
    #[test]
    fn a_source_that_comes_back_is_continued() {
        let s = store(&[
            "100-mine",
            "200-mine",
            "850-profile:home:0001-a",
            "snitchwatch-allow-x",
            "z00-blocklist:ads:0001-a",
            "z00-blocklist:trackers:0001-b",
        ]);
        let labels: Vec<_> = (0..s.len())
            .map(|i| section_label(&s, i).unwrap())
            .collect();
        assert_eq!(
            labels,
            [
                USER_RULES,
                USER_RULES,
                PROFILE_RULES,
                "User rules (continued)",
                BLOCKLIST_RULES,
                BLOCKLIST_RULES,
            ]
        );
        assert_eq!(section_label(&s, 6), None);
    }
}
