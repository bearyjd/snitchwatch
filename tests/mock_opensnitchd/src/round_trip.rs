//! What opensnitchd v1.8.0 reports back for a rule it was sent, so tests
//! can feed the bridge the daemon's shape rather than the exact rule sent
//! (code review M4 of PR #105).
//!
//! The path is `Deserialize` (a list's `data` cleared) → `replaceUserRule`
//! (`Compile`, for an enabled rule: a list's operand set to `list`, a
//! case-insensitive regexp's data lowercased in place) → `Save` to disk →
//! load → `Serialize` (`created` from the file; a rule loaded from disk can
//! carry its list JSON in the list's `data`, which `Serialize` copies before
//! clearing). See `vendor/opensnitch/daemon/rule/{rule,operator,loader}.go`.

use snitchwatch_proto::protocol::{Operator, Rule};

/// `rule` as the daemon reports it after a restart.
pub fn as_daemon_reports(rule: &Rule) -> Rule {
    let mut reported = rule.clone();
    reported.created = 1_700_000_000;
    if let Some(op) = reported.operator.as_mut() {
        if op.r#type == "list" {
            op.operand = "list".into();
            op.data = list_json(&op.list);
        }
        if rule.enabled {
            lowercase_insensitive_regexps(op);
        }
    }
    reported
}

fn lowercase_insensitive_regexps(op: &mut Operator) {
    if op.r#type == "regexp" && !op.sensitive {
        op.data = op.data.to_lowercase();
    }
    for member in &mut op.list {
        lowercase_insensitive_regexps(member);
    }
}

/// The list's JSON, as the stock UI writes it into `data`.
fn list_json(list: &[Operator]) -> String {
    let members: Vec<serde_json::Value> = list
        .iter()
        .map(|op| {
            serde_json::json!({
                "type": op.r#type,
                "operand": op.operand,
                "data": op.data,
                "sensitive": op.sensitive,
            })
        })
        .collect();
    serde_json::Value::Array(members).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_differs_from_what_was_sent_the_way_the_daemon_does() {
        let sent = Rule {
            name: "x".into(),
            enabled: true,
            created: 0,
            operator: Some(Operator {
                r#type: "list".into(),
                operand: String::new(),
                list: vec![Operator {
                    r#type: "regexp".into(),
                    operand: "protocol".into(),
                    data: "^TCP$".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let reported = as_daemon_reports(&sent);
        let op = reported.operator.unwrap();
        assert_eq!(op.operand, "list");
        assert!(op.data.starts_with('['));
        assert_eq!(op.list[0].data, "^tcp$");
        assert_eq!(reported.created, 1_700_000_000);
    }
}
