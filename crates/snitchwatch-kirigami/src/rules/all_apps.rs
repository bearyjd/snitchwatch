//! Issue #44, second half (Part B): find Snitchwatch prompt rules saved before
//! #50 that match **every** program, so the Rules tab can flag them with a
//! one-click delete per row. No automatic migration, no bulk delete.
//!
//! Detection is by provenance plus operator shape, never by name. Every
//! Snitchwatch prompt rule carries `verdict_to_rule`'s description; before
//! #50 its host scopes emitted a bare `dest.host` / `dest.ip` simple operator
//! or a `regexp` `dest.host` one, with no `process.path` anywhere. A name
//! pattern can't tell a host starting with `p` (`…-allow-pypi.org-<hex>-443`)
//! from #50's `-p<program>-<hex>` component, so names are not consulted.
//!
//! Why the deny warning matters: opensnitchd stops at the first matching deny
//! but keeps scanning after a non-precedence allow (`vendor:daemon/rule/
//! loader.go` `FindFirstMatch`), so a host-only deny overrides every allow
//! covering that host. Deleting it unblocks the host for each app with such
//! an allow — including one for just this host — and other apps are asked.

use super::row_store::{Rule, RulesStore};
use snitchwatch_bridge::translator::verdict::strip_display_hazards;

/// The description `snitchwatch_bridge::translator::verdict::verdict_to_rule`
/// gives every interactive prompt rule (since M1.5).
const INTERACTIVE_VERDICT_DESCRIPTION: &str = "snitchwatch interactive verdict";

impl Rule {
    /// A Snitchwatch prompt rule whose operator matches only a destination,
    /// so it applies to every program.
    pub fn applies_to_all_apps(&self) -> bool {
        self.all_apps_target().is_some()
    }

    /// What deleting a flagged rule changes, in plain text with the
    /// destination's display hazards removed; `None` when not flagged.
    pub fn all_apps_hint(&self) -> Option<String> {
        let target = self.all_apps_target()?;
        Some(match self.normalized_action() {
            "allow" => format!("Deleting this makes every app ask again before reaching {target}."),
            _ => format!(
                "Deleting this unblocks {target} for every app with an allow rule covering it, \
                 including rules for just this host; other apps will be asked."
            ),
        })
    }

    /// The destination a flagged rule covers, as display text.
    fn all_apps_target(&self) -> Option<String> {
        if self.description != INTERACTIVE_VERDICT_DESCRIPTION
            || mentions_process_path(&self.operator)
        {
            return None;
        }
        host_only_target(&self.operator).map(|target| strip_display_hazards(&target))
    }
}

impl RulesStore {
    /// How many rules [`Rule::applies_to_all_apps`] flags.
    pub fn legacy_host_only_count(&self) -> usize {
        self.rules()
            .iter()
            .filter(|r| r.applies_to_all_apps())
            .count()
    }
}

/// Whether a `process.path` operand appears anywhere in `operator`.
fn mentions_process_path(operator: &serde_json::Value) -> bool {
    match operator {
        serde_json::Value::Object(map) => {
            map.get("operand").and_then(|v| v.as_str()) == Some("process.path")
                || map.values().any(mentions_process_path)
        }
        serde_json::Value::Array(items) => items.iter().any(mentions_process_path),
        _ => false,
    }
}

/// For a pre-#50 host scope's operator (one leaf: a `list` has type `list`),
/// the destination it covers; `None` for any other shape.
fn host_only_target(operator: &serde_json::Value) -> Option<String> {
    let field = |key: &str| operator.get(key).and_then(|v| v.as_str());
    let data = field("data").filter(|d| !d.is_empty())?;
    match (field("type")?, field("operand")?) {
        ("simple", "dest.host" | "dest.ip") => Some(data.to_string()),
        ("regexp", "dest.host") => Some(describe_domain_pattern(data)),
        _ => None,
    }
}

/// The two domain patterns Snitchwatch's "Any host on this domain" scope has
/// emitted, named readably; never the raw pattern for anything else.
fn describe_domain_pattern(pattern: &str) -> String {
    let parent = |prefix: &str| {
        pattern
            .strip_prefix(prefix)?
            .strip_suffix('$')
            .and_then(unescape_literal)
    };
    if let Some(domain) = parent(r"^(?:[^.]+\.)*") {
        format!("{domain} and its subdomains")
    } else if let Some(domain) = parent(r"^.*\.") {
        format!("subdomains of {domain}")
    } else {
        "the hosts this rule matches".to_string()
    }
}

/// `regex::escape`'s inverse for a pattern that is one plain literal; `None`
/// if it contains any unescaped metacharacter.
fn unescape_literal(escaped: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = escaped.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' => {
                return None
            }
            other => out.push(other),
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snitchwatch_bridge::ws_messages::ServerMessage;

    const VERDICT: &str = INTERACTIVE_VERDICT_DESCRIPTION;

    fn rule(name: &str, action: &str, description: &str, operator: serde_json::Value) -> Rule {
        Rule {
            name: name.to_string(),
            enabled: true,
            action: action.to_string(),
            duration: "always".to_string(),
            description: description.to_string(),
            operator,
            ..Default::default()
        }
    }

    fn simple(operand: &str, data: &str) -> serde_json::Value {
        json!({"type": "simple", "operand": operand, "data": data, "sensitive": false})
    }

    fn host(name: &str, action: &str, host: &str) -> Rule {
        rule(name, action, VERDICT, simple("dest.host", host))
    }

    #[test]
    fn pre_50_host_only_rules_are_flagged_whatever_their_name() {
        let flagged = [
            host(
                "snitchwatch-allow-github.com-3aeb002460381c6f-443",
                "allow",
                "github.com",
            ),
            // A host starting with `p` must not pass for #50's `-p<program>`.
            host(
                "snitchwatch-allow-pypi.org-1b4f0e9851971998-443",
                "allow",
                "pypi.org",
            ),
            host("snitchwatch-deny-github.com-443", "deny", "github.com"),
            host(
                "snitchwatch-allow-github.com-3aeb002460381c6f-443-2",
                "allow",
                "github.com",
            ),
            rule(
                "snitchwatch-deny-www.example.com-80fc0fb9266db7b8-443",
                "deny",
                VERDICT,
                json!({"type": "regexp", "operand": "dest.host",
                       "data": r"^(?:[^.]+\.)*example\.com$", "sensitive": false}),
            ),
            rule(
                "snitchwatch-deny-140.82.121.4-5b8c1e0e04c2d4a4-443",
                "deny",
                VERDICT,
                simple("dest.ip", "140.82.121.4"),
            ),
        ];
        for r in &flagged {
            assert!(r.applies_to_all_apps(), "{}", r.name);
        }
    }

    #[test]
    fn app_bound_foreign_and_blocklist_rules_are_not_flagged() {
        let not_flagged = [
            // #50: process.path AND host.
            rule(
                "snitchwatch-allow-github.com-3aeb002460381c6f-443-pcurl-0123456789abcdef",
                "allow",
                VERDICT,
                json!({"type": "list", "operands": [
                    {"type": "simple", "operand": "process.path", "data": "/usr/bin/curl",
                     "sensitive": true},
                    simple("dest.host", "github.com"),
                ]}),
            ),
            // Pre-#50 `AnyHost`: already bound to its program.
            rule(
                "snitchwatch-allow-github.com-3aeb002460381c6f-443",
                "allow",
                VERDICT,
                simple("process.path", "/usr/bin/curl"),
            ),
            // Same shape, not Snitchwatch's: hand-written or stock UI.
            rule(
                "snitchwatch-allow-github.com-3aeb002460381c6f-443",
                "allow",
                "",
                simple("dest.host", "github.com"),
            ),
            rule(
                "z00-blocklist:ads:0001-tracker.example",
                "deny",
                r#"{"blocklist":"ads"}"#,
                simple("dest.host", "tracker.example"),
            ),
            // Host-shaped but not a shape any Snitchwatch prompt emitted.
            rule(
                "snitchwatch-deny-x",
                "deny",
                VERDICT,
                json!({"type": "list", "operands": [simple("dest.host", "github.com")]}),
            ),
            rule(
                "snitchwatch-deny-y",
                "deny",
                VERDICT,
                simple("dest.port", "443"),
            ),
            // A process.path anywhere disqualifies it, whatever the top level.
            rule(
                "snitchwatch-deny-w",
                "deny",
                VERDICT,
                json!({"type": "simple", "operand": "dest.host", "data": "github.com",
                       "operands": [{"type": "simple", "operand": "process.path",
                                     "data": "/usr/bin/curl"}]}),
            ),
            rule(
                "snitchwatch-deny-z",
                "deny",
                VERDICT,
                serde_json::Value::Null,
            ),
        ];
        for r in &not_flagged {
            assert!(!r.applies_to_all_apps(), "{} {}", r.name, r.operator);
            assert_eq!(r.all_apps_hint(), None, "{}", r.name);
        }
    }

    #[test]
    fn a_flagged_deny_warns_that_deleting_it_unblocks_the_host() {
        let hint = host("snitchwatch-deny-github.com-443", "deny", "github.com")
            .all_apps_hint()
            .expect("flagged");
        assert_eq!(
            hint,
            "Deleting this unblocks github.com for every app with an allow rule covering it, \
             including rules for just this host; other apps will be asked."
        );
        let reject = host("snitchwatch-deny-github.com-443", "reject", "github.com");
        assert!(reject
            .all_apps_hint()
            .unwrap()
            .starts_with("Deleting this unblocks"));
    }

    #[test]
    fn a_flagged_allow_says_every_app_will_ask_again() {
        let hint = host(
            "snitchwatch-allow-pypi.org-1b4f0e9851971998-443",
            "allow",
            "pypi.org",
        )
        .all_apps_hint()
        .expect("flagged");
        assert_eq!(
            hint,
            "Deleting this makes every app ask again before reaching pypi.org."
        );
    }

    #[test]
    fn the_destination_is_named_readably_and_without_display_hazards() {
        let domain = |data: &str| {
            rule(
                "snitchwatch-deny-d",
                "allow",
                VERDICT,
                json!({"type": "regexp", "operand": "dest.host", "data": data}),
            )
            .all_apps_hint()
            .unwrap()
        };
        assert!(domain(r"^(?:[^.]+\.)*example\.com$").contains(" example.com and its subdomains."));
        assert!(domain(r"^.*\.example\.com$").contains(" subdomains of example.com."));
        // Never a raw pattern shown as if it were a host.
        assert!(domain(r"^(a|b)\.example\.com$").contains(" the hosts this rule matches."));
        let ip = rule("i", "allow", VERDICT, simple("dest.ip", "140.82.121.4"));
        assert!(ip.all_apps_hint().unwrap().ends_with(" 140.82.121.4."));
        let hostile = host("h", "allow", "evil\u{202e}moc.example\u{200b}");
        assert!(hostile
            .all_apps_hint()
            .unwrap()
            .ends_with(" evilmoc.example."));
    }

    #[test]
    fn the_store_counts_flagged_rules() {
        let mut store = RulesStore::new();
        store.apply(&ServerMessage::SetRules {
            rules: vec![
                serde_json::to_value(host("a", "allow", "github.com")).unwrap(),
                serde_json::to_value(host("b", "deny", "pypi.org")).unwrap(),
                serde_json::to_value(rule("c", "allow", VERDICT, simple("process.path", "/x")))
                    .unwrap(),
            ],
        });
        assert_eq!(store.legacy_host_only_count(), 2);
    }
}
