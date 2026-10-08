//! The JSON shape a rule takes between the bridge and its GUIs (`SetRules`,
//! `UpdateRules`, and the rule a GUI sends back in `AddRule`/`UpdateRule`),
//! and its conversion to and from opensnitchd's proto `Rule`.
//!
//! Split out of `grpc_server.rs` (issue #48) so the rules cache doesn't
//! depend on the gRPC service module.

use snitchwatch_proto::protocol::Rule;

/// Why a GUI may not edit a rule whose name fails
/// [`crate::rule_name::validate_rule_name`] (e.g. one the stock OpenSnitch
/// UI saved with a `\` or a very long name). Such a rule is still listed,
/// because opensnitchd still enforces it, but the bridge never sends a
/// command for it, so the GUI shows it read-only with this reason.
pub const READ_ONLY_REASON: &str = "Snitchwatch can't change or delete this rule because of its \
     name (too long, or containing characters Snitchwatch won't send back to the firewall \
     service). The rule still applies.";

/// Convert a rule returned to opensnitchd into the tolerant wire shape the
/// desktop Rules model consumes. Persistent interactive verdicts originate in
/// this gRPC reply, rather than in an upstream `SetRules` push, so without
/// this conversion the daemon saves a rule the UI never learns about.
pub fn rule_to_wire(rule: &Rule) -> serde_json::Value {
    serde_json::json!({
        "name": rule.name,
        "enabled": rule.enabled,
        "action": rule.action,
        "duration": rule.duration,
        "description": rule.description,
        "operator": rule.operator.as_ref().map(operator_to_wire).unwrap_or(serde_json::Value::Null),
        // Round-trip ballast, not display data: the Rules model sends the whole
        // rule back as a CHANGE_RULE and the daemon does a wholesale `Replace`,
        // so any field omitted here is a field the next toggle silently clears
        // on the daemon. Dropping `precedence` would quietly change which rule
        // wins for unrelated traffic. See `rule_from_wire`, which reads both.
        "precedence": rule.precedence,
        "nolog": rule.nolog,
        // Display only: `name` stays exact because commands identify the
        // rule by it, while daemon-sourced names may carry bidi overrides or
        // zero-width characters that make a row read as a different rule.
        "displayName": crate::translator::verdict::strip_display_hazards(&rule.name),
        // `null` for every rule a GUI may edit.
        "readOnlyReason": crate::rule_policy::read_only_reason(rule),
        // Separate from `readOnlyReason`: a rule read-only only for its
        // conditions can still be deleted by name.
        "deletable": crate::rule_policy::deletable(rule),
        // Whether a GUI may turn it on or off: a recommended rule is
        // read-only but can still be toggled.
        "toggleable": crate::rule_policy::toggleable(rule),
    })
}

pub(crate) fn operator_to_wire(
    operator: &snitchwatch_proto::protocol::Operator,
) -> serde_json::Value {
    if operator.list.is_empty() {
        serde_json::json!({
            "type": operator.r#type,
            "operand": operator.operand,
            "data": operator.data,
            "sensitive": operator.sensitive,
        })
    } else {
        serde_json::json!({
            "type": operator.r#type,
            "operands": operator.list.iter().map(operator_to_wire).collect::<Vec<_>>(),
        })
    }
}

/// Inverse of [`rule_to_wire`]: parse the wire rule shape the Rules model
/// emits (see `snitchwatch-kirigami`'s `rules::row_store::Rule`) back into the
/// proto [`Rule`] opensnitchd expects in a `CHANGE_RULE` notification.
///
/// **Returns `Err` rather than ever producing `operator: None`.** The daemon
/// runs every notified rule through `rule.Deserialize`
/// (`vendor/opensnitch/daemon/rule/rule.go:85-89`, which hard-rejects a null
/// operator) and then `Operator.Compile()`
/// (`vendor/opensnitch/daemon/rule/operator.go:109-214`, which rejects unknown
/// operator types and uncompilable regexps). A rejected rule doesn't error
/// visibly — the daemon just falls back to its default action. That silent
/// failure is exactly what issue #14 was, so malformed input dies here instead.
///
/// `created` is left at 0 (the daemon stamps its own). `precedence`/`nolog` are
/// read when present and default to `false`; see this module's
/// `rule_from_wire` tests for the round-trip guarantee.
pub(crate) fn rule_from_wire(v: &serde_json::Value) -> Result<Rule, String> {
    let obj = v.as_object().ok_or("rule must be a JSON object")?;
    let name = obj
        .get("name")
        .and_then(|x| x.as_str())
        .ok_or("rule.name missing")?;
    crate::rule_name::validate_rule_name(name)?;
    let action = obj
        .get("action")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("rule.action missing or empty")?;
    let operator = obj.get("operator").ok_or("rule.operator missing")?;
    if operator.is_null() {
        return Err("rule.operator is null; the daemon would reject this rule".to_string());
    }
    let operator = operator_from_wire(operator)?;
    crate::rule_policy::validate_operator(&operator)?;

    Ok(Rule {
        created: 0,
        name: name.to_string(),
        description: str_field(obj, "description"),
        enabled: bool_field(obj, "enabled"),
        precedence: bool_field(obj, "precedence"),
        nolog: bool_field(obj, "nolog"),
        action: action.to_string(),
        duration: obj
            .get("duration")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .ok_or("rule.duration missing or empty")?
            .to_string(),
        operator: Some(operator),
    })
}

/// Inverse of [`operator_to_wire`], mirroring its two branches: an `operands`
/// array means a list operator, anything else is a leaf.
pub(crate) fn operator_from_wire(
    v: &serde_json::Value,
) -> Result<snitchwatch_proto::protocol::Operator, String> {
    let obj = v.as_object().ok_or("operator must be a JSON object")?;
    let op_type = obj
        .get("type")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("operator.type missing or empty")?
        .to_string();

    match obj.get("operands").and_then(|x| x.as_array()) {
        Some(operands) => {
            // A GUI controls the size: bound it before converting anything.
            // `rule_policy::validate_operator` checks the result again.
            let max = crate::cache::rules::MAX_OPERATOR_LIST_LEN;
            if operands.len() > max {
                return Err(format!(
                    "a list operator has {} members; the limit is {max}",
                    operands.len()
                ));
            }
            if operands
                .iter()
                .any(|m| m.get("operands").and_then(|x| x.as_array()).is_some())
            {
                return Err("a list can't contain a list".to_string());
            }
            Ok(snitchwatch_proto::protocol::Operator {
                r#type: op_type,
                list: operands
                    .iter()
                    .map(operator_from_wire)
                    .collect::<Result<Vec<_>, _>>()?,
                ..Default::default()
            })
        }
        None => Ok(snitchwatch_proto::protocol::Operator {
            r#type: op_type,
            operand: obj
                .get("operand")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .ok_or("operator.operand missing or empty")?
                .to_string(),
            data: str_field(obj, "data"),
            sensitive: bool_field(obj, "sensitive"),
            list: Vec::new(),
        }),
    }
}

fn str_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    obj.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

fn bool_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    obj.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Rule {
        Rule {
            name: name.to_string(),
            action: "deny".into(),
            duration: "always".into(),
            operator: Some(snitchwatch_proto::protocol::Operator {
                r#type: "simple".into(),
                operand: "dest.host".into(),
                data: "example.com".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Decision (b) of the #48 review: a daemon rule with a name the bridge
    /// refuses to send back is listed read-only, with a reason, not hidden.
    #[test]
    fn a_name_the_bridge_will_not_send_back_is_read_only_with_a_reason() {
        let long = "a".repeat(crate::rule_name::MAX_RULE_NAME_BYTES + 1);
        for name in [r"stock\ui", "a/b", "..", long.as_str(), "bidi\u{202e}name"] {
            let wire = rule_to_wire(&named(name));
            assert_eq!(wire["name"], name, "the exact name is kept");
            assert_eq!(wire["readOnlyReason"], READ_ONLY_REASON, "{name:?}");
        }
        let editable = rule_to_wire(&named("899-firefox-allow-out"));
        assert!(editable["readOnlyReason"].is_null());
    }

    #[test]
    fn display_name_drops_bidi_and_zero_width_characters() {
        let wire = rule_to_wire(&named("evil\u{202e}txt\u{200b}.exe"));
        assert_eq!(wire["displayName"], "eviltxt.exe");
    }

    /// A GUI controls the list's size: it is checked before any member is
    /// converted, so a huge or nested list costs nothing.
    #[test]
    fn list_size_and_nesting_are_checked_before_members_are_converted() {
        let wire = |operator: serde_json::Value| {
            serde_json::json!({
                "name": "899-x", "action": "deny", "duration": "always", "operator": operator,
            })
        };
        // Members that aren't even objects: the size is what's reported.
        let too_many =
            vec![serde_json::Value::Null; crate::cache::rules::MAX_OPERATOR_LIST_LEN + 1];
        let err = rule_from_wire(&wire(
            serde_json::json!({ "type": "list", "operands": too_many }),
        ))
        .unwrap_err();
        assert!(err.contains("64"), "{err}");

        let nested = serde_json::json!({
            "type": "list",
            "operands": [
                { "type": "list", "operands": [null] },
            ],
        });
        let err = rule_from_wire(&wire(nested)).unwrap_err();
        assert!(err.contains("list can't contain a list"), "{err}");

        // `"operands": null` is not a list, as in the conversion itself.
        let null_operands = serde_json::json!({
            "type": "list",
            "operands": [
                { "type": "simple", "operand": "process.path", "data": "/x", "operands": null },
            ],
        });
        assert!(rule_from_wire(&wire(null_operands)).is_ok());
    }

    /// The fetch rule the system image ships is listed in both forms the
    /// daemon reports (on disk, and compiled with the uid in `user.name`),
    /// read-only with fixed text, not deletable, and never sent back.
    #[test]
    fn the_packaged_fetch_rule_is_listed_read_only_and_never_sent_back() {
        use crate::rule_name::PACKAGED_FETCH_RULE_NAME;
        use crate::translator::rule_notification::notification_for_effect;
        use crate::translator::upstream::UpstreamEffect;
        use test_helpers::packaged_fetch_rule;

        for uid in [None, Some("987")] {
            let rule = packaged_fetch_rule(uid);
            let wire = rule_to_wire(&rule);
            assert_eq!(wire["name"], PACKAGED_FETCH_RULE_NAME);
            assert_eq!(
                wire["readOnlyReason"],
                crate::rule_policy::PACKAGED_FETCH_RULE_REASON,
                "{uid:?}"
            );
            assert_eq!(wire["deletable"], false, "{uid:?}");
            assert_eq!(wire["precedence"], false);
            assert_eq!(wire["operator"]["operands"].as_array().unwrap().len(), 4);
            // A GUI echoing the row back (a toggle) is refused before the daemon.
            let effect = UpstreamEffect::UpdateRule {
                rule_id: PACKAGED_FETCH_RULE_NAME.to_string(),
                rule: wire,
            };
            assert!(notification_for_effect(&effect, 1).is_err(), "{uid:?}");
        }
        assert!(
            crate::daemon_commands::BlocklistCommand::delete(PACKAGED_FETCH_RULE_NAME).is_none()
        );
    }
}

#[cfg(test)]
pub mod test_helpers {
    use snitchwatch_proto::protocol::{Operator, Rule};

    pub use crate::rule_name::PACKAGED_FETCH_RULE_NAME;

    /// `packaging/bluebuild/files/system/etc/opensnitchd/rules/` — the rule
    /// Snitchwatch ships for the system bridge's blocklist downloads.
    pub const PACKAGED_FETCH_RULE_JSON: &str = include_str!(
        "../../../packaging/bluebuild/files/system/etc/opensnitchd/rules/000-snitchwatch-bridge-fetch.json"
    );

    /// The shipped rule as opensnitchd's `Serialize` reports it. With
    /// `compiled_uid`, the `user.name` member holds that uid, as `Compile`
    /// leaves it for an enabled rule; `None` is the file as written.
    pub fn packaged_fetch_rule(compiled_uid: Option<&str>) -> Rule {
        let json: serde_json::Value = serde_json::from_str(PACKAGED_FETCH_RULE_JSON).unwrap();
        let text = |v: &serde_json::Value, key: &str| v[key].as_str().unwrap().to_string();
        let leaf = |v: &serde_json::Value| {
            let mut data = text(v, "data");
            if let (Some(uid), "user.name") = (compiled_uid, v["operand"].as_str().unwrap()) {
                data = uid.to_string();
            }
            Operator {
                r#type: text(v, "type"),
                operand: text(v, "operand"),
                data,
                sensitive: v["sensitive"].as_bool().unwrap(),
                list: Vec::new(),
            }
        };
        let operator = &json["operator"];
        Rule {
            created: 0,
            name: text(&json, "name"),
            description: text(&json, "description"),
            enabled: json["enabled"].as_bool().unwrap(),
            precedence: json["precedence"].as_bool().unwrap(),
            nolog: json["nolog"].as_bool().unwrap(),
            action: text(&json, "action"),
            duration: text(&json, "duration"),
            operator: Some(Operator {
                r#type: text(operator, "type"),
                operand: text(operator, "operand"),
                data: String::new(),
                sensitive: operator["sensitive"].as_bool().unwrap(),
                list: operator["list"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(leaf)
                    .collect(),
            }),
        }
    }
}
