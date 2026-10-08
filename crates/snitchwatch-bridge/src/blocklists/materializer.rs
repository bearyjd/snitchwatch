//! The opensnitchd rules a blocklist subscription becomes (issue #45 PR B).
//!
//! One non-precedence **deny** rule per subscription and list kind, using
//! opensnitchd's own `lists.*` operators, each pointing at a directory the
//! bridge writes ([`crate::blocklists::list_dir`]):
//!
//! | Rule name | Operator | `data` |
//! |---|---|---|
//! | `z00-blocklist:<list>:domains` | `lists` / `lists.domains` | `<state>/blocklists/<list>/domains` |
//! | `z00-blocklist:<list>:ips` | `lists` / `lists.ips` | `<state>/blocklists/<list>/ips` |
//!
//! The `ips` rule exists only while the list has IPv4 entries. Each kind
//! needs its own directory: `readLists` (`vendor:daemon/rule/
//! operator_lists.go`) loads every `*.*` file in `data` with the rule's own
//! parser, so a shared directory would feed the domains file to the IP rule.
//!
//! **The blocklist wins.** opensnitchd's `FindFirstMatch`
//! (`vendor:daemon/rule/loader.go`) keeps scanning after a non-precedence
//! allow and returns on the first matching deny, so a blocklist deny beats
//! every allow except a `precedence: true` one, whatever the rule names sort
//! as. The `z00` band only keeps these rules together, after the numeric
//! user bands; it does not make user rules win.
//!
//! ## Legacy names
//!
//! Earlier builds named one rule per host, `z00-blocklist:<list>:<seq>-<host>`
//! or, before that, `900-blocklist:<list>:…`. No release installed any (their
//! sink was a no-op), but the daemon sink still deletes every rule under
//! [`owned_rule_name_prefixes`] that isn't one of the names above.

use std::path::Path;

use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol;

use crate::blocklists::list_dir::IdComponent;
use crate::rule_name::{BLOCKLIST_RULE_NAME_PREFIX, LEGACY_BLOCKLIST_RULE_NAME_PREFIX};

/// Plain-data shape of one rule, converted to the proto at the send point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedRule {
    pub name: String,
    pub enabled: bool,
    pub action: String,
    pub duration: String,
    pub description: String,
    pub operator: Operator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operator {
    #[serde(rename = "type")]
    pub kind: String,
    pub operand: String,
    pub data: String,
    pub sensitive: bool,
}

impl From<MaterializedRule> for protocol::Rule {
    fn from(rule: MaterializedRule) -> Self {
        protocol::Rule {
            created: 0,
            name: rule.name,
            description: rule.description,
            enabled: rule.enabled,
            precedence: false,
            nolog: false,
            action: rule.action,
            duration: rule.duration,
            operator: Some(protocol::Operator {
                r#type: rule.operator.kind,
                operand: rule.operator.operand,
                data: rule.operator.data,
                sensitive: rule.operator.sensitive,
                list: Vec::new(),
            }),
        }
    }
}

/// Which `lists.*` operator a rule uses, and so which directory and file it
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ListKind {
    /// Host names, as `0.0.0.0 <host>` lines (`filterDomains`).
    Domains,
    /// IPv4 addresses, one per line (`readSimpleList`), matched against the
    /// connection's `DstIP.String()`.
    Ips,
}

impl ListKind {
    pub const ALL: [ListKind; 2] = [ListKind::Domains, ListKind::Ips];

    pub fn operand(self) -> &'static str {
        match self {
            ListKind::Domains => "lists.domains",
            ListKind::Ips => "lists.ips",
        }
    }

    /// The kind's directory under a list's directory, and the rule name's
    /// last segment.
    pub fn dir_name(self) -> &'static str {
        match self {
            ListKind::Domains => "domains",
            ListKind::Ips => "ips",
        }
    }

    /// The one file in the kind's directory. The daemon only loads names
    /// with a dot that don't start with one (`<dir>/*.*`, hidden skipped).
    pub fn file_name(self) -> &'static str {
        match self {
            ListKind::Domains => "domains.list",
            ListKind::Ips => "ips.list",
        }
    }

    pub fn from_operand(operand: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.operand() == operand)
    }
}

/// `z00-blocklist:<list>:<kind>`, at most 14 + 81 + 1 + 7 bytes.
pub fn list_rule_name(list: &IdComponent, kind: ListKind) -> String {
    format!("{BLOCKLIST_RULE_NAME_PREFIX}{list}:{}", kind.dir_name())
}

/// Every name prefix under which one list's rules may exist on a daemon:
/// the current band and the legacy one.
pub fn owned_rule_name_prefixes(list: &IdComponent) -> [String; 2] {
    [
        format!("{BLOCKLIST_RULE_NAME_PREFIX}{list}:"),
        format!("{LEGACY_BLOCKLIST_RULE_NAME_PREFIX}{list}:"),
    ]
}

/// The list a blocklist rule name belongs to (its second `:` segment), if
/// the name is under a blocklist prefix.
pub fn list_of_rule_name(name: &str) -> Option<&str> {
    let rest = name
        .strip_prefix(BLOCKLIST_RULE_NAME_PREFIX)
        .or_else(|| name.strip_prefix(LEGACY_BLOCKLIST_RULE_NAME_PREFIX))?;
    rest.split(':').next()
}

/// The deny rule for one list kind. `dir` is the kind's directory, as
/// [`crate::blocklists::list_dir::ListDir::kind_dir`] builds it: absolute,
/// canonical, no trailing slash.
pub fn materialize_list_rule(list: &IdComponent, kind: ListKind, dir: &Path) -> MaterializedRule {
    let description = serde_json::json!({
        "snitchwatch": {
            "source": "blocklist",
            "list_id": list.as_str(),
            "kind": kind.dir_name(),
        }
    })
    .to_string();
    MaterializedRule {
        name: list_rule_name(list, kind),
        enabled: true,
        action: "deny".to_string(),
        duration: "always".to_string(),
        description,
        operator: Operator {
            kind: "lists".to_string(),
            operand: kind.operand().to_string(),
            data: dir.to_string_lossy().into_owned(),
            sensitive: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::specificity::BLOCKLIST_BAND_PREFIX;

    fn list(id: &str) -> IdComponent {
        IdComponent::from_id(id)
    }

    #[test]
    fn one_deny_rule_per_kind_points_at_its_own_directory() {
        let ads = list("ads-0123456789abcdef");
        let dir = Path::new("/var/lib/snitchwatch/blocklists/ads-0123456789abcdef/domains");
        let rule = materialize_list_rule(&ads, ListKind::Domains, dir);
        assert_eq!(rule.name, "z00-blocklist:ads-0123456789abcdef:domains");
        assert_eq!(
            (rule.action.as_str(), rule.duration.as_str(), rule.enabled),
            ("deny", "always", true)
        );
        assert_eq!(rule.operator.kind, "lists");
        assert_eq!(rule.operator.operand, "lists.domains");
        assert_eq!(rule.operator.data, dir.to_str().unwrap());
        assert!(!rule.operator.sensitive);

        let ips = materialize_list_rule(&ads, ListKind::Ips, Path::new("/x/ips"));
        assert_eq!(ips.name, "z00-blocklist:ads-0123456789abcdef:ips");
        assert_eq!(ips.operator.operand, "lists.ips");
    }

    #[test]
    fn the_proto_rule_is_a_non_precedence_leaf() {
        let rule: protocol::Rule =
            materialize_list_rule(&list("ads"), ListKind::Domains, Path::new("/x/domains")).into();
        assert!(!rule.precedence && !rule.nolog);
        let op = rule.operator.unwrap();
        assert!(op.list.is_empty());
        assert_eq!(op.r#type, "lists");
    }

    #[test]
    fn the_band_is_the_reserved_prefix_and_sorts_after_user_rules() {
        assert_eq!(
            format!("{BLOCKLIST_BAND_PREFIX}00-blocklist:"),
            BLOCKLIST_RULE_NAME_PREFIX
        );
        let name = list_rule_name(&list("ads"), ListKind::Domains);
        assert!(crate::rule_name::is_reserved_blocklist_name(&name));
        assert!(name.as_str() > "999");
    }

    #[test]
    fn names_fit_the_daemon_and_name_their_list() {
        let longest = list(&"a".repeat(81));
        for kind in ListKind::ALL {
            let name = list_rule_name(&longest, kind);
            assert!(
                crate::rule_name::validate_rule_name(&name).is_ok(),
                "{name}"
            );
            assert_eq!(list_of_rule_name(&name), Some(longest.as_str()));
        }
        assert_eq!(list_of_rule_name("900-blocklist:ads:0001-x"), Some("ads"));
        assert_eq!(list_of_rule_name("899-firefox"), None);
    }

    #[test]
    fn owned_prefixes_cover_both_bands_of_one_list_only() {
        let prefixes = owned_rule_name_prefixes(&list("ads"));
        assert_eq!(prefixes, ["z00-blocklist:ads:", "900-blocklist:ads:"]);
        assert!(!"z00-blocklist:ads2:domains".starts_with(&prefixes[0]));
    }

    #[test]
    fn kinds_round_trip_through_their_operand() {
        for kind in ListKind::ALL {
            assert_eq!(ListKind::from_operand(kind.operand()), Some(kind));
            assert!(kind.file_name().contains('.') && !kind.file_name().starts_with('.'));
        }
        assert_eq!(ListKind::from_operand("lists.nets"), None);
    }
}
