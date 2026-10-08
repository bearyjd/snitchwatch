//! Blocklist (`lists.*`) rules as opensnitchd sees them (issue #45 PR B).
//!
//! - [`load_list_dir`] ports `readLists` (`vendor/opensnitch/daemon/rule/
//!   operator_lists.go`): which files a `lists` rule's directory loads, and
//!   which keys it ends up matching.
//! - [`validate_blocklist_rule`] checks a bridge-built blocklist rule against
//!   the path contract and returns what the daemon would load.
//! - [`spawn_responder`] answers the bridge's commands like a daemon:
//!   accepting everything, or refusing `lists` rules from the UI channel the
//!   way bazzite-tower's patched daemon does until it has the path contract.

use std::collections::BTreeSet;
use std::path::Path;

use snitchwatch_proto::protocol::{
    Action, Notification, NotificationReply, NotificationReplyCode, Rule,
};
use tokio::sync::mpsc;

use crate::{validate_rule_shape, MockError};

/// What a refusing daemon answers a `lists` rule with.
pub const LISTS_REFUSAL: &str = "lists operators are not accepted from the UI";

/// `filterDomains`: the host a hosts-format line contributes, if any.
pub fn filter_domains(line: &str) -> Option<&str> {
    if line.len() < 9 {
        return None;
    }
    let host = if line.starts_with("127.0.0.1") {
        line.get(10..)?
    } else if line.starts_with("0.0.0.0") {
        &line[8..]
    } else {
        return None;
    };
    match host {
        "local" | "localhost" | "localhost.localdomain" | "broadcasthost" => None,
        host => Some(host),
    }
}

/// `readLists` for `operand`: every `<dir>/*.*` file not starting with a
/// dot, merged into one key set.
pub fn load_list_dir(operand: &str, dir: &Path) -> std::io::Result<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !name.contains('.') {
            continue;
        }
        let raw = std::fs::read_to_string(entry.path())?;
        for line in raw.split('\n') {
            let key = match operand {
                "lists.domains" => filter_domains(line).map(str::trim),
                _ if line.is_empty() || line.starts_with('#') => None,
                _ => Some(line.trim()),
            };
            keys.extend(key.map(str::to_string));
        }
    }
    Ok(keys)
}

/// A blocklist rule as the path contract allows it: a non-precedence,
/// always, enabled deny named `z00-blocklist:<list>:<kind>`, whose `lists`
/// operator reads `<lists_root>/<list>/<kind>`, a real directory. Returns
/// what the daemon would load from it.
#[allow(clippy::result_large_err)]
pub fn validate_blocklist_rule(
    rule: &Rule,
    lists_root: &Path,
) -> Result<BTreeSet<String>, MockError> {
    validate_rule_shape(rule)?;
    let bad = |why: &str| MockError::InvalidRule(format!("{}: {why}", rule.name));
    let op = rule.operator.as_ref().ok_or_else(|| bad("no operator"))?;
    let kind = match op.operand.as_str() {
        "lists.domains" => "domains",
        "lists.ips" => "ips",
        _ => return Err(bad("not a lists.domains/lists.ips operand")),
    };
    if op.r#type != "lists" || op.sensitive || !op.list.is_empty() {
        return Err(bad("not a plain lists operator"));
    }
    if rule.action != "deny" || rule.duration != "always" || !rule.enabled || rule.precedence {
        return Err(bad("not an enabled, always, non-precedence deny"));
    }
    let dir = Path::new(&op.data);
    let list = dir
        .strip_prefix(lists_root)
        .map_err(|_| bad("data outside the lists root"))?;
    let parts: Vec<_> = list.iter().filter_map(|p| p.to_str()).collect();
    if parts.len() != 2 || parts[1] != kind || op.data.ends_with('/') {
        return Err(bad("data is not <root>/<list>/<kind>"));
    }
    if rule.name != format!("z00-blocklist:{}:{kind}", parts[0]) {
        return Err(bad("name doesn't match its directory"));
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| bad(&e.to_string()))?;
    if !meta.is_dir() {
        return Err(bad("data is not a real directory"));
    }
    load_list_dir(&op.operand, dir).map_err(|e| bad(&e.to_string()))
}

/// How [`spawn_responder`] answers.
#[derive(Debug, Clone)]
pub enum ListsPolicy {
    /// `OK` to every well-formed command.
    Accept,
    /// `ERROR` with this text to every `CHANGE_RULE` carrying a `lists`
    /// operator; `OK` to the rest.
    RefuseLists(String),
}

fn is_lists_change(n: &Notification) -> bool {
    n.r#type == Action::ChangeRule as i32
        && n.rules.iter().any(|rule| {
            rule.operator
                .as_ref()
                .is_some_and(|op| op.r#type == "lists" || op.operand.starts_with("lists."))
        })
}

/// Answer every command arriving on `inbound` (from
/// [`crate::MockOpensnitchd::open_notifications`]) through `replies`, and
/// forward each command, unchanged, to the returned receiver. A
/// `CHANGE_RULE` the daemon would reject outright gets `ERROR` too.
pub fn spawn_responder(
    policy: ListsPolicy,
    replies: mpsc::Sender<NotificationReply>,
    mut inbound: mpsc::Receiver<Notification>,
) -> mpsc::Receiver<Notification> {
    let (seen_tx, seen_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Some(n) = inbound.recv().await {
            let refusal = match &policy {
                ListsPolicy::RefuseLists(text) if is_lists_change(&n) => Some(text.clone()),
                _ if n.r#type == Action::ChangeRule as i32 => n
                    .rules
                    .iter()
                    .find_map(|rule| validate_rule_shape(rule).err())
                    .map(|e| e.to_string()),
                _ => None,
            };
            let reply = NotificationReply {
                id: n.id,
                code: if refusal.is_some() {
                    NotificationReplyCode::Error as i32
                } else {
                    NotificationReplyCode::Ok as i32
                },
                data: refusal.unwrap_or_default(),
            };
            let _ = seen_tx.send(n).await;
            if replies.send(reply).await.is_err() {
                return;
            }
        }
    });
    seen_rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_domains_matches_the_daemon() {
        assert_eq!(filter_domains("0.0.0.0 ads.example"), Some("ads.example"));
        assert_eq!(filter_domains("127.0.0.1 ads.example"), Some("ads.example"));
        assert_eq!(filter_domains("0.0.0.0 localhost"), None);
        assert_eq!(filter_domains("ads.example"), None);
        assert_eq!(
            filter_domains("127.0.0.1"),
            None,
            "the daemon would panic here"
        );
        assert_eq!(filter_domains("1.2.3.4"), None);
    }

    #[test]
    fn a_directory_loads_only_dotted_visible_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.list"), "0.0.0.0 a.example\n").unwrap();
        std::fs::write(dir.path().join(".a.list.tmp"), "0.0.0.0 tmp.example\n").unwrap();
        std::fs::write(dir.path().join("nodot"), "0.0.0.0 nodot.example\n").unwrap();
        std::fs::write(dir.path().join("plain.txt"), "plain.example\n").unwrap();
        let keys = load_list_dir("lists.domains", dir.path()).unwrap();
        assert_eq!(keys.into_iter().collect::<Vec<_>>(), vec!["a.example"]);
        let ips = load_list_dir("lists.ips", dir.path()).unwrap();
        assert!(ips.contains("plain.example") && ips.contains("0.0.0.0 a.example"));
    }
}
