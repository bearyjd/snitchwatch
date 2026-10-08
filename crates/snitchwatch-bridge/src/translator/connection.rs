//! Translate an opensnitchd `Connection` proto into a `ConnectionRow` for
//! the WebSocket layer.
//!
//! The `notification_id` argument is the daemon-supplied id we want to use
//! as a stable correlation handle so the WS client can later send back a
//! `setVerdict` referencing the same row.

use crate::daemon_contract::{is_contract_default_action, is_default_action_rule};
use crate::translator::verdict::is_answer_name;
use crate::ws_messages::ConnectionRow;
use snitchwatch_proto::protocol::{Connection, Event};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const ASK_ROW_PREFIX: &str = "ask-";
/// Id prefix for rows synthesized from a daemon-reported `Event` (see
/// [`event_to_row`]) — connections the daemon already decided, by an
/// existing rule or (with the bazzite-tower fork) by its default action, and
/// reports via `Statistics.events` on a `Ping` call, as opposed to
/// `ASK_ROW_PREFIX` rows the daemon is actively prompting for. The full id is
/// `event-<unixnano>-<seq>`; nothing parses it.
pub const EVENT_ROW_PREFIX: &str = "event-";

/// The `<seq>` of the next event row id. The daemon's time alone can repeat,
/// and an id must name one row: "Make a rule…" finds its row by id when
/// clicked (PR #108 security review, L2).
static NEXT_EVENT_ROW: AtomicU64 = AtomicU64::new(1);

/// Whether an off-contract default-action event was logged this run.
static UNEXPECTED_DEFAULT_ACTION_LOGGED: AtomicBool = AtomicBool::new(false);

pub fn ask_row_id(notification_id: u64) -> String {
    format!("{ASK_ROW_PREFIX}{notification_id}")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn connection_to_row(conn: &Connection, notification_id: u64) -> ConnectionRow {
    let process = if conn.process_path.is_empty() {
        "<unknown>".to_string()
    } else {
        std::path::Path::new(&conn.process_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string()
    };

    let process_path = if conn.process_path.is_empty() {
        None
    } else {
        Some(conn.process_path.clone())
    };

    let dst_host = if conn.dst_host.is_empty() {
        conn.dst_ip.clone()
    } else {
        conn.dst_host.clone()
    };

    ConnectionRow {
        id: ask_row_id(notification_id),
        process,
        process_path,
        dst_host,
        dst_ip: conn.dst_ip.clone(),
        dst_port: conn.dst_port as u16,
        protocol: conn.protocol.clone(),
        direction: "outgoing".to_string(),
        action: None,
        bytes_sent: 0,
        bytes_received: 0,
        // The moment this row was built, i.e. the moment the connection
        // became pending — `event_to_row` immediately overwrites this with
        // the daemon's own `event.unixnano` for already-decided rows, so
        // this default only ever surfaces for genuinely-pending AskRule
        // rows, where it anchors the pending-decision-exposure warning
        // (see docs/superpowers/plans/2026-08-05-pending-decision-exposure-warning.md).
        started_at_ms: now_ms(),
        // An AskRule row is, by construction, a connection opensnitchd found
        // no existing rule for (that's exactly why it's asking) — there is no
        // matched rule yet. `ConnectionCache::resolve` fills this in once the
        // user's verdict becomes the governing rule.
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    }
}

/// Normalize a daemon-reported rule action string the same way
/// `snitchwatch-kirigami`'s `rules::row_store::Rule::normalized_action` does:
/// exactly `"allow"` or `"deny"`, folding anything else (opensnitchd's
/// `"reject"` included) into `"deny"`.
fn normalized_action(action: &str) -> &'static str {
    if action.eq_ignore_ascii_case("allow") {
        "allow"
    } else {
        "deny"
    }
}

/// Translate a daemon-reported `Event` (a `Connection` paired with the `Rule`
/// that decided it) into a *decided* `ConnectionRow` carrying that rule's
/// name in `matched_rule`. A default-action event
/// (`daemon_contract::is_default_action_rule`) instead has no `matched_rule` and is marked
/// `decided_by_default`, with the action the default applied.
///
/// The daemon includes recent `Event`s in `Statistics.events` on its
/// periodic `Ping` calls — this is how the bridge learns about connections
/// that matched a pre-existing rule, or (fork) got the default action, and
/// therefore never went through the interactive `AskRule` flow or went
/// through it unanswered (see `grpc_server::UiService::ping`). Returns
/// `None` when the event doesn't carry both a connection and a rule — there
/// is nothing useful to show without both.
///
/// A once answer isn't listed twice (issue #102): the bridge's own `once`
/// answer to an Ask decided only the connection the bridge was asked about,
/// which it already lists as that Ask's row (labelled, for an answer the
/// filtering pause gave), and the daemon never stores a `once` rule. So such
/// an event is left out, but only when `listed` says the committed rule list
/// has no rule of that name (PR #106 review, security L2): with no list
/// (`None`), or a stored rule of that name, the row is listed. The daemon's
/// own `once` rules (`ui.client.*`, decided with no Ask row) are listed.
pub fn event_to_row(event: &Event, listed: impl Fn(&str) -> Option<bool>) -> Option<ConnectionRow> {
    let conn = event.connection.as_ref()?;
    let rule = event.rule.as_ref()?;
    if rule.duration == "once" && is_answer_name(&rule.name) && listed(&rule.name) == Some(false) {
        return None;
    }
    let by_default = is_default_action_rule(rule);

    if by_default {
        note_unexpected_default_action(&UNEXPECTED_DEFAULT_ACTION_LOGGED, &rule.action);
    }

    let mut row = connection_to_row(conn, 0);
    row.id = format!(
        "{EVENT_ROW_PREFIX}{}-{}",
        event.unixnano,
        NEXT_EVENT_ROW.fetch_add(1, Ordering::Relaxed)
    );
    row.action = Some(normalized_action(&rule.action).to_string());
    row.matched_rule = (!by_default).then(|| rule.name.clone());
    row.decided_by_default = by_default;
    row.started_at_ms = event.unixnano / 1_000_000;
    Some(row)
}

/// Logs, once per `logged` flag (once per bridge run in production), a
/// default-action event whose action the contract doesn't name. The row
/// still folds it like any event action ([`normalized_action`]). Returns
/// whether it logged.
fn note_unexpected_default_action(logged: &AtomicBool, action: &str) -> bool {
    if is_contract_default_action(action) || logged.swap(true, Ordering::Relaxed) {
        return false;
    }
    tracing::warn!(
        action = %crate::translator::verdict::sanitize_for_display(action, 32),
        shown_as = normalized_action(action),
        "a default-action event carried an action outside the contract (allow, deny, \
         reject); later ones are not logged"
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_contract::DEFAULT_ACTION_MARKER;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn sample_connection() -> Connection {
        Connection {
            protocol: "tcp".to_string(),
            src_ip: "192.168.1.10".to_string(),
            src_port: 51544,
            dst_ip: "140.82.121.4".to_string(),
            dst_host: "github.com".to_string(),
            dst_port: 443,
            user_id: 1000,
            process_id: 4242,
            process_path: "/usr/bin/curl".to_string(),
            process_cwd: "/home/alice".to_string(),
            process_args: vec!["curl".into(), "https://github.com".into()],
            process_env: Default::default(),
            process_checksums: Default::default(),
            process_tree: vec![],
        }
    }

    #[test]
    fn ask_row_id_is_stable() {
        assert_eq!(ask_row_id(7), "ask-7");
    }

    #[test]
    fn connection_to_row_populates_all_visible_fields() {
        let conn = sample_connection();
        let row = connection_to_row(&conn, 42);

        assert_eq!(row.id, "ask-42");
        assert_eq!(row.process, "curl");
        assert_eq!(row.process_path.as_deref(), Some("/usr/bin/curl"));
        assert_eq!(row.dst_host, "github.com");
        assert_eq!(row.dst_ip, "140.82.121.4");
        assert_eq!(row.dst_port, 443);
        assert_eq!(row.protocol, "tcp");
        assert_eq!(row.direction, "outgoing");
        assert!(row.action.is_none(), "ask-rule rows start pending");
        // The `Connection` proto carries no byte counters (issue #19) — this
        // is a documented limitation of the upstream wire shape, not a
        // desired zero value. The Traffic tab is built around daemon
        // aggregate `Statistics` instead; see `ws_messages::ServerMessage::
        // DaemonStatistics`.
        assert_eq!(row.bytes_sent, 0);
        assert_eq!(row.bytes_received, 0);
    }

    #[test]
    fn connection_with_no_dst_host_falls_back_to_ip() {
        let mut conn = sample_connection();
        conn.dst_host = String::new();
        let row = connection_to_row(&conn, 5);
        assert_eq!(row.dst_host, "140.82.121.4");
    }

    #[test]
    fn process_basename_is_extracted_from_path() {
        let mut conn = sample_connection();
        conn.process_path = "/opt/firefox/firefox".to_string();
        let row = connection_to_row(&conn, 1);
        assert_eq!(row.process, "firefox");
    }

    #[test]
    fn process_with_empty_path_uses_unknown() {
        let mut conn = sample_connection();
        conn.process_path = String::new();
        let row = connection_to_row(&conn, 1);
        assert_eq!(row.process, "<unknown>");
        assert_eq!(row.process_path, None);
    }

    #[test]
    fn ask_rows_start_with_no_matched_rule() {
        let row = connection_to_row(&sample_connection(), 1);
        assert!(row.matched_rule.is_none());
    }

    #[test]
    fn ask_rows_are_stamped_with_the_current_time() {
        let before = now_ms();
        let row = connection_to_row(&sample_connection(), 1);
        let after = now_ms();
        assert!(
            row.started_at_ms >= before && row.started_at_ms <= after,
            "expected started_at_ms in [{before}, {after}], got {}",
            row.started_at_ms
        );
    }

    fn sample_rule(name: &str, action: &str) -> snitchwatch_proto::protocol::Rule {
        snitchwatch_proto::protocol::Rule {
            created: 1_700_000_000,
            name: name.to_string(),
            description: String::new(),
            enabled: true,
            precedence: false,
            nolog: false,
            action: action.to_string(),
            duration: "always".to_string(),
            operator: None,
        }
    }

    #[test]
    fn event_to_row_carries_the_matched_rule_name_and_decided_action() {
        let event = Event {
            time: "2026-07-05T12:00:00Z".to_string(),
            connection: Some(sample_connection()),
            rule: Some(sample_rule("899-firefox-allow-out.json", "allow")),
            unixnano: 1_700_000_000_123_456_789,
        };
        let row = event_to_row(&event, |_| Some(false)).expect("both connection and rule present");
        assert!(
            row.id.starts_with("event-1700000000123456789-"),
            "{}",
            row.id
        );
        assert_eq!(row.process, "curl");
        assert_eq!(row.dst_host, "github.com");
        assert_eq!(row.action.as_deref(), Some("allow"));
        assert_eq!(
            row.matched_rule.as_deref(),
            Some("899-firefox-allow-out.json")
        );
        assert_eq!(row.started_at_ms, 1_700_000_000_123);
    }

    #[test]
    fn event_to_row_folds_reject_action_to_deny() {
        let event = Event {
            time: String::new(),
            connection: Some(sample_connection()),
            rule: Some(sample_rule(
                "z00-blocklist:ads:0001-tracker.example",
                "reject",
            )),
            unixnano: 1,
        };
        let row = event_to_row(&event, |_| Some(false)).unwrap();
        assert_eq!(row.action.as_deref(), Some("deny"));
    }

    #[test]
    fn event_to_row_is_none_without_a_connection() {
        let event = Event {
            time: String::new(),
            connection: None,
            rule: Some(sample_rule("899-firefox-allow-out.json", "allow")),
            unixnano: 1,
        };
        assert!(event_to_row(&event, |_| Some(false)).is_none());
    }

    /// Issue #102: a once answer isn't listed twice. A `once` rule is only
    /// ever an answer to an Ask (the daemon never stores one), so its event
    /// is the asked connection again, already listed as its Ask row with
    /// that row's label ("Allowed once (filtering was paused)", say).
    #[test]
    fn a_once_answer_to_an_ask_is_not_listed_twice() {
        for (name, action) in [
            ("snitchwatch-allow-github.com-443-0123abcd", "allow"),
            ("snitchwatch-deny-github.com-443-0123abcd", "deny"),
        ] {
            let mut once = sample_rule(name, action);
            once.duration = "once".into();
            let event = Event {
                time: String::new(),
                connection: Some(sample_connection()),
                rule: Some(once),
                unixnano: 1,
            };
            assert!(event_to_row(&event, |_| Some(false)).is_none(), "{name}");
        }
        let mut kept = sample_rule("snitchwatch-allow-github.com-443-0123abcd", "allow");
        kept.duration = "until restart".into();
        let event = Event {
            time: String::new(),
            connection: Some(sample_connection()),
            rule: Some(kept),
            unixnano: 1,
        };
        assert!(
            event_to_row(&event, |_| Some(true)).is_some(),
            "a stored rule's events are listed"
        );
        // The daemon's own once rules (no GUI connected, an Ask that
        // failed) have no Ask row in the bridge: listed.
        let mut daemons = sample_rule("ui.client.disconnected", "allow");
        daemons.duration = "once".into();
        let event = Event {
            rule: Some(daemons),
            ..event
        };
        assert!(event_to_row(&event, |_| Some(false)).is_some());
    }

    /// PR #106 review, security L2: only an answer the list is known not to
    /// hold is left out. With no list, or a stored rule of that name (a
    /// file someone named like an answer), the event is listed.
    #[test]
    fn a_once_answer_is_listed_unless_the_list_is_known_not_to_have_it() {
        let mut once = sample_rule("snitchwatch-deny-github.com-443-0123abcd", "deny");
        once.duration = "once".into();
        let event = Event {
            time: String::new(),
            connection: Some(sample_connection()),
            rule: Some(once),
            unixnano: 1,
        };
        assert!(event_to_row(&event, |_| None).is_some(), "no list");
        assert!(event_to_row(&event, |_| Some(true)).is_some(), "stored");
        let asked = std::cell::Cell::new(None);
        event_to_row(&event, |name| {
            asked.set(Some(name.to_string()));
            Some(false)
        });
        assert_eq!(
            asked.take().as_deref(),
            Some("snitchwatch-deny-github.com-443-0123abcd")
        );
    }

    /// The bazzite-tower fork's synthetic rule for a connection that got the
    /// daemon's `DefaultAction` (plan `2026-10-08-default-applied-events.md`).
    fn default_action_rule(action: &str) -> snitchwatch_proto::protocol::Rule {
        snitchwatch_proto::protocol::Rule {
            created: 1_700_000_000,
            name: String::new(),
            description: DEFAULT_ACTION_MARKER.to_string(),
            enabled: true,
            precedence: false,
            nolog: false,
            action: action.to_string(),
            duration: "once".to_string(),
            operator: Some(snitchwatch_proto::protocol::Operator {
                r#type: "simple".to_string(),
                operand: "true".to_string(),
                data: String::new(),
                sensitive: false,
                list: vec![],
            }),
        }
    }

    fn event_with(rule: snitchwatch_proto::protocol::Rule) -> Event {
        Event {
            time: String::new(),
            connection: Some(sample_connection()),
            rule: Some(rule),
            unixnano: 1_700_000_000_123_456_789,
        }
    }

    #[test]
    fn a_default_action_event_is_decided_by_the_default_not_a_rule() {
        for (applied, shown) in [("allow", "allow"), ("deny", "deny"), ("reject", "deny")] {
            let event = event_with(default_action_rule(applied));
            assert!(is_default_action_rule(event.rule.as_ref().unwrap()));
            let row = event_to_row(&event, |_| Some(false)).expect("connection and rule present");
            assert_eq!(row.action.as_deref(), Some(shown), "{applied}");
            assert_eq!(row.matched_rule, None, "{applied}: no rule named \"\"");
            assert!(row.decided_by_default, "{applied}");
            assert!(!row.deferred);
            assert!(
                row.id.starts_with("event-1700000000123456789-"),
                "{}",
                row.id
            );
            assert_eq!(row.started_at_ms, 1_700_000_000_123);
        }
    }

    #[test]
    fn an_unmarked_rule_named_empty_keeps_todays_row() {
        // Stock v1.8.0 can load a hand-written rule file named "": its
        // events are real rule hits.
        let mut rule = default_action_rule("deny");
        rule.description = String::new();
        assert!(!is_default_action_rule(&rule));
        let row = event_to_row(&event_with(rule), |_| Some(false)).unwrap();
        assert_eq!(row.matched_rule.as_deref(), Some(""));
        assert!(!row.decided_by_default);
        // Only the exact marker counts.
        let mut rule = default_action_rule("deny");
        rule.description = "Snitchwatch:default-action ".to_string();
        assert!(!is_default_action_rule(&rule));
    }

    #[test]
    fn a_named_rule_with_the_marker_description_is_an_ordinary_rule() {
        let mut rule = default_action_rule("allow");
        rule.name = "copied-description".to_string();
        assert!(!is_default_action_rule(&rule));
        let row = event_to_row(&event_with(rule), |_| Some(false)).unwrap();
        assert_eq!(row.matched_rule.as_deref(), Some("copied-description"));
        assert!(!row.decided_by_default);
    }

    #[test]
    fn rule_rows_and_ask_rows_are_not_decided_by_default() {
        let event = event_with(sample_rule("899-firefox-allow-out.json", "allow"));
        assert!(
            !event_to_row(&event, |_| Some(false))
                .unwrap()
                .decided_by_default
        );
        assert!(!connection_to_row(&sample_connection(), 1).decided_by_default);
    }

    /// Pinned: an action outside the contract folds like any event action,
    /// "deny" unless it is "allow" (PR #108 review).
    #[test]
    fn a_default_action_event_with_an_unexpected_action_folds_to_deny() {
        for action in ["drop", "", "ACCEPT"] {
            let row =
                event_to_row(&event_with(default_action_rule(action)), |_| Some(false)).unwrap();
            assert_eq!(row.action.as_deref(), Some("deny"), "{action:?}");
            assert!(row.decided_by_default, "{action:?}");
        }
    }

    #[test]
    fn an_unexpected_default_action_is_logged_once() {
        let logged = AtomicBool::new(false);
        for action in ["allow", "deny", "reject"] {
            assert!(!note_unexpected_default_action(&logged, action));
        }
        assert!(
            !logged.load(Ordering::Relaxed),
            "a contract action doesn't use up the warning"
        );
        assert!(note_unexpected_default_action(&logged, "drop"));
        assert!(!note_unexpected_default_action(&logged, "other"));
    }

    /// L2 (PR #108 security review): two events with the same daemon time
    /// are two rows, and "Make a rule…" finds a row by id.
    #[test]
    fn event_rows_have_distinct_ids_even_at_the_same_time() {
        let event = event_with(sample_rule("899-firefox-allow-out.json", "allow"));
        let first = event_to_row(&event, |_| Some(false)).unwrap();
        let second = event_to_row(&event, |_| Some(false)).unwrap();
        assert_ne!(first.id, second.id);
        for row in [&first, &second] {
            assert!(
                row.id.starts_with("event-1700000000123456789-"),
                "{}",
                row.id
            );
        }
    }

    #[test]
    fn event_to_row_is_none_without_a_rule() {
        let event = Event {
            time: String::new(),
            connection: Some(sample_connection()),
            rule: None,
            unixnano: 1,
        };
        assert!(event_to_row(&event, |_| Some(false)).is_none());
    }
}
