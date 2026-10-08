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
use snitchwatch_bridge::translator::verdict::{sanitize_for_display, strip_display_hazards};

/// The description `snitchwatch_bridge::translator::verdict::verdict_to_rule`
/// gives every interactive prompt rule (since M1.5).
const INTERACTIVE_VERDICT_DESCRIPTION: &str = "snitchwatch interactive verdict";

/// Longest destination shown in a hint (`sanitize_for_display` adds `…`).
const MAX_TARGET_CHARS: usize = 64;

/// The row badges: a destination-only rule, and one tied to a "program"
/// that isn't a program file (issue #64).
pub const ALL_APPS_BADGE: &str = "Applies to all apps";
pub const UNIDENTIFIED_BADGE: &str = "Program not identified";

impl Rule {
    /// Flagged on the Rules page: [`Self::applies_to_all_apps`], or a
    /// Snitchwatch prompt rule tied to a process path that doesn't name a
    /// real program file (issue #64; #44 Part A refuses those for new
    /// rules: the daemon's `Kernel connection` placeholder, a bare name,
    /// `/proc/self/exe`, a memfd).
    pub fn flagged(&self) -> bool {
        self.applies_to_all_apps() || self.unidentified_program().is_some()
    }

    /// The flagged row's badge (empty when not flagged).
    pub fn flag_badge(&self) -> &'static str {
        if self.applies_to_all_apps() {
            ALL_APPS_BADGE
        } else if self.unidentified_program().is_some() {
            UNIDENTIFIED_BADGE
        } else {
            ""
        }
    }

    /// The process path a Snitchwatch prompt rule is tied to that isn't a
    /// program file, as plain text for a `PlainText` label: display hazards
    /// stripped, nothing escaped, at most [`MAX_TARGET_CHARS`].
    fn unidentified_program(&self) -> Option<String> {
        if self.description != INTERACTIVE_VERDICT_DESCRIPTION {
            return None;
        }
        unbindable_path(&self.operator).map(|path| plain_truncated(&path))
    }

    /// What deleting a rule tied to an unidentified program changes.
    fn unidentified_hint(&self, path: &str) -> String {
        let deny = self.normalized_action() != "allow";
        let does = if deny { "blocks" } else { "allows" };
        let tied = if path.is_empty() {
            format!(
                "Tied to an empty program path, so it {does} whatever the firewall reports \
                 with no program, not one app."
            )
        } else {
            format!(
                "Tied to \"{path}\", which isn't a program file, so it {does} whatever the \
                 firewall reports under that name, not one app."
            )
        };
        if !self.can_delete() {
            format!("{tied} Snitchwatch can't delete it; its details say why.")
        } else if !self.enabled {
            format!("{tied} It is disabled; deleting it removes it for good.")
        } else {
            format!("{tied} Deleting it makes those connections ask again.")
        }
    }
    /// A Snitchwatch prompt rule whose operator matches only a destination,
    /// so it applies to every program.
    pub fn applies_to_all_apps(&self) -> bool {
        self.all_apps_target().is_some()
    }

    /// What deleting a flagged rule changes, as plain text with the
    /// destination sanitized for display; `None` when not flagged. A rule
    /// Snitchwatch can't delete (`Rule::can_delete`, the bridge's `deletable`
    /// flag that `RulesStore::is_deletable` and the row's Delete button use) or a
    /// disabled one is described as it is, never as "Deleting this ...".
    pub fn all_apps_hint(&self) -> Option<String> {
        let Some(target) = self.all_apps_target() else {
            return self
                .unidentified_program()
                .map(|path| self.unidentified_hint(&path));
        };
        let deny = self.normalized_action() != "allow";
        Some(if !self.can_delete() {
            "This rule applies to every app. Snitchwatch can't delete it; its details say why."
                .to_string()
        } else if !self.enabled && deny {
            format!(
                "Disabled, so it blocks nothing now. Re-enabled, it would block {target} for \
                 every app; deleting it removes it for good."
            )
        } else if !self.enabled {
            "Disabled, so it allows nothing now; deleting it removes it for good.".to_string()
        } else if deny {
            format!(
                "Deleting this unblocks {target} for every app with an allow rule covering it, \
                 including rules for just this host; other apps will be asked."
            )
        } else {
            format!("Deleting this makes every app ask again before reaching {target}.")
        })
    }

    /// The destination a flagged rule covers, as display text.
    fn all_apps_target(&self) -> Option<String> {
        if self.description != INTERACTIVE_VERDICT_DESCRIPTION
            || mentions_process_path(&self.operator)
        {
            return None;
        }
        host_only_target(&self.operator)
    }
}

impl RulesStore {
    /// How many rules [`Rule::flagged`] flags.
    pub fn legacy_host_only_count(&self) -> usize {
        self.rules().iter().filter(|r| r.flagged()).count()
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

/// `text` without display hazards, cut to [`MAX_TARGET_CHARS`] with `…`.
fn plain_truncated(text: &str) -> String {
    let plain = strip_display_hazards(text);
    match plain.char_indices().nth(MAX_TARGET_CHARS) {
        Some((cut, _)) => format!("{}…", &plain[..cut]),
        None => plain,
    }
}

/// The first `simple` `process.path` value in `operator` that isn't a real
/// program file's path (`is_bindable_process_path`, #44 Part A).
fn unbindable_path(operator: &serde_json::Value) -> Option<String> {
    use snitchwatch_bridge::translator::process_binding::is_bindable_process_path;
    let leaves: Vec<&serde_json::Value> = match operator.get("operands").and_then(|o| o.as_array())
    {
        Some(members) => members.iter().collect(),
        None => vec![operator],
    };
    leaves.into_iter().find_map(|leaf| {
        let field = |key: &str| leaf.get(key).and_then(|v| v.as_str());
        let data = field("data")?;
        (field("type") == Some("simple")
            && field("operand") == Some("process.path")
            && !is_bindable_process_path(data))
        .then(|| data.to_string())
    })
}

/// For a pre-#50 host scope's operator (one leaf: a `list` has type `list`),
/// the destination it covers, sanitized for display (the host is DNS/SNI-
/// influenced, #44 security review S2); `None` for any other shape.
fn host_only_target(operator: &serde_json::Value) -> Option<String> {
    let field = |key: &str| operator.get(key).and_then(|v| v.as_str());
    let data = field("data").filter(|d| !d.is_empty())?;
    match (field("type")?, field("operand")?) {
        ("simple", "dest.host" | "dest.ip") => Some(sanitize_for_display(data, MAX_TARGET_CHARS)),
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
            .map(|domain| sanitize_for_display(&domain, MAX_TARGET_CHARS))
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
        // Security review S2: DNS/SNI-influenced, so bounded and markup-safe
        // like every other display boundary.
        let long = host("l", "deny", &format!("{}.example", "a".repeat(100)));
        let hint = long.all_apps_hint().unwrap();
        assert!(
            hint.contains(&format!(" {}… for every app", "a".repeat(64))),
            "{hint}"
        );
        let markup = host("m", "allow", "<b>x</b>.example");
        assert!(markup
            .all_apps_hint()
            .unwrap()
            .ends_with(" &lt;b&gt;x&lt;/b&gt;.example."));
    }

    /// Code review C5: a disabled rule changes nothing now, and a read-only
    /// one has no Delete button, so neither may say "Deleting this ...".
    #[test]
    fn disabled_and_read_only_rows_are_described_as_they_are() {
        let mut disabled_deny = host("d", "deny", "github.com");
        disabled_deny.enabled = false;
        assert_eq!(
            disabled_deny.all_apps_hint().unwrap(),
            "Disabled, so it blocks nothing now. Re-enabled, it would block github.com for \
             every app; deleting it removes it for good."
        );
        let mut disabled_allow = host("a", "allow", "pypi.org");
        disabled_allow.enabled = false;
        assert_eq!(
            disabled_allow.all_apps_hint().unwrap(),
            "Disabled, so it allows nothing now; deleting it removes it for good."
        );
        let mut locked = host("r", "deny", "github.com");
        locked.enabled = false;
        locked.read_only_reason = Some("Snitchwatch can't edit this rule.".into());
        assert_eq!(
            locked.all_apps_hint().unwrap(),
            "This rule applies to every app. Snitchwatch can't delete it; its details say why."
        );
        // #68: a rule refused only for its shape is read-only but deletable,
        // so its Delete button shows and the hint says what deleting does.
        let mut shape_refused = host("s", "deny", "github.com");
        shape_refused.read_only_reason = Some("Snitchwatch can't edit this rule.".into());
        shape_refused.deletable = Some(true);
        assert!(shape_refused
            .all_apps_hint()
            .unwrap()
            .starts_with("Deleting this unblocks github.com"));
        let mut name_refused = host("n", "allow", "github.com");
        name_refused.deletable = Some(false);
        assert!(name_refused
            .all_apps_hint()
            .unwrap()
            .contains("can't delete it"));
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

    /// The fetch rule the system image ships, as opensnitchd reports it:
    /// enabled with `compiled_uid` (the uid `Compile` writes over
    /// `user.name`), or disabled with the file's own data (`None`).
    fn packaged_fetch_rule(compiled_uid: Option<&str>) -> snitchwatch_proto::protocol::Rule {
        use snitchwatch_proto::protocol::{Operator, Rule as DaemonRule};
        let json: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../packaging/bluebuild/files/system/etc/opensnitchd/rules/000-snitchwatch-bridge-fetch.json"
        ))
        .unwrap();
        let text = |v: &serde_json::Value, key: &str| v[key].as_str().unwrap().to_string();
        let leaf = |v: &serde_json::Value| Operator {
            r#type: text(v, "type"),
            operand: text(v, "operand"),
            data: match (compiled_uid, v["operand"].as_str()) {
                (Some(uid), Some("user.name")) => uid.to_string(),
                _ => text(v, "data"),
            },
            sensitive: v["sensitive"].as_bool().unwrap(),
            list: Vec::new(),
        };
        DaemonRule {
            name: text(&json, "name"),
            description: text(&json, "description"),
            enabled: compiled_uid.is_some(),
            precedence: json["precedence"].as_bool().unwrap(),
            action: text(&json, "action"),
            duration: text(&json, "duration"),
            operator: Some(Operator {
                r#type: "list".into(),
                operand: "list".into(),
                list: json["operator"]["list"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(leaf)
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Fed through the bridge's own rules cache and wire shape: bound to a
    /// program, so never flagged as applying to every app. Its name is
    /// reserved for rules built into Snitchwatch, so it is read-only with
    /// fixed text, can't be deleted, and no toggle is built for it.
    #[test]
    fn the_packaged_fetch_rule_is_never_flagged_and_built_in() {
        use super::super::row_store::RuleSource;
        use snitchwatch_bridge::cache::rules::RulesCache;
        use snitchwatch_bridge::rule_policy::PACKAGED_FETCH_RULE_REASON;

        for compiled_uid in [Some("987"), None] {
            let mut cache = RulesCache::default();
            cache.replace_all(vec![packaged_fetch_rule(compiled_uid)]);
            let mut store = RulesStore::new();
            store.apply(&ServerMessage::SetRules {
                rules: cache.snapshot_wire().unwrap(),
            });
            let r = store.find_by_name("000-snitchwatch-bridge-fetch").unwrap();
            assert!(!r.applies_to_all_apps());
            assert_eq!(r.all_apps_hint(), None);
            assert_eq!(store.legacy_host_only_count(), 0);
            assert_eq!(r.source(), RuleSource::User);
            assert_eq!(r.normalized_action(), "allow");
            assert!(!r.precedence);
            assert_eq!(
                r.read_only_reason.as_deref(),
                Some(PACKAGED_FETCH_RULE_REASON)
            );
            assert!(!r.can_delete(), "{compiled_uid:?}");
            assert!(!store.is_deletable(&r.name), "{compiled_uid:?}");
            assert!(store.rule_json_with_enabled(&r.name, false).is_none());
            assert_eq!(
                r.operator_summary(),
                format!(
                    "process.path = /usr/bin/snitchwatch-bridge-cli AND user.name = {} \
                     AND dest.port = 443 AND protocol = ^tcp6?$",
                    compiled_uid.unwrap_or("snitchwatch")
                )
            );
        }
    }

    /// Issue #64: a Snitchwatch prompt rule saved before #44 Part A could
    /// be tied to a "program" that isn't a program file (the daemon's
    /// `Kernel connection` placeholder, a bare name, `/proc/self/exe`); it
    /// is flagged like an all-apps rule, with its own badge and hint.
    #[test]
    fn rules_tied_to_an_unidentified_program_are_flagged() {
        let tied = |path: &str, action: &str| {
            rule(
                "snitchwatch-allow-github.com-443-px",
                action,
                VERDICT,
                json!({"type": "list", "operands": [
                    {"type": "simple", "operand": "process.path", "data": path, "sensitive": true},
                    simple("dest.host", "github.com"),
                ]}),
            )
        };
        for path in [
            "Kernel connection",
            "curl",
            "/proc/self/exe",
            "/memfd:x (deleted)",
            "<unknown>",
            "a&b",
        ] {
            let r = tied(path, "allow");
            assert!(r.flagged(), "{path}");
            assert!(!r.applies_to_all_apps(), "{path}");
            assert_eq!(r.flag_badge(), UNIDENTIFIED_BADGE);
            let hint = r.all_apps_hint().unwrap();
            assert!(hint.contains("isn't a program file"), "{hint}");
            // A PlainText label: shown as it is, never HTML-escaped.
            assert!(hint.contains(&format!("\"{path}\"")), "{hint}");
        }
        let empty = tied("", "allow").all_apps_hint().unwrap();
        assert!(
            empty.starts_with("Tied to an empty program path"),
            "{empty}"
        );
        let hazard = tied("cu\u{202e}rl", "allow").all_apps_hint().unwrap();
        assert!(hazard.contains("\"curl\""), "{hazard}");
        let long = tied(&"x".repeat(100), "allow").all_apps_hint().unwrap();
        assert!(
            long.contains(&format!("\"{}…\"", "x".repeat(MAX_TARGET_CHARS))),
            "{long}"
        );
        let deny = tied("Kernel connection", "deny").all_apps_hint().unwrap();
        assert!(deny.contains("blocks"), "{deny}");
        let named = tied("/usr/bin/curl", "allow");
        assert!(!named.flagged() && named.all_apps_hint().is_none());
        let foreign = Rule {
            description: String::new(),
            ..tied("Kernel connection", "allow")
        };
        assert!(!foreign.flagged(), "only Snitchwatch's own prompt rules");
        let all_apps = host("snitchwatch-allow-github.com-443", "allow", "github.com");
        assert_eq!(all_apps.flag_badge(), ALL_APPS_BADGE);
        let mut store = RulesStore::new();
        store.apply(&ServerMessage::SetRules {
            rules: vec![
                serde_json::to_value(tied("Kernel connection", "allow")).unwrap(),
                serde_json::to_value(all_apps).unwrap(),
            ],
        });
        assert_eq!(store.legacy_host_only_count(), 2);
    }
}
