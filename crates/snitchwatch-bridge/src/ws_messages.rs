//! Serde structs for the 22 LS WebSocket message types.
//!
//! All server-to-client messages share the same envelope: `{action: "...", ...}`.
//! We model this as a tagged enum for round-trip type safety.

use serde::{Deserialize, Serialize};

/// Which readiness/connectivity property a diagnostic check covers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    DaemonReachable,
    FirewallRunning,
    EbpfSupport,
    NftablesSupport,
}

/// Result of one diagnostic check. `Unknown` covers "can't assess yet"
/// (e.g. opensnitchd connected but hasn't sent a `ClientConfig` yet) —
/// never reported as `Ok` or `Failed` when the bridge genuinely doesn't
/// know.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Failed { detail: String },
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticCheck {
    pub kind: CheckKind,
    pub status: CheckStatus,
}

/// Server → client message envelope. Each variant matches one of the 22
/// `handleServerCommand` cases in the LS UI's `app.js`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerMessage {
    InsertConnectionRows {
        rows: Vec<ConnectionRow>,
    },
    UpdateConnectionRows {
        rows: Vec<ConnectionRow>,
    },
    RemoveConnectionRows {
        ids: Vec<String>,
    },
    /// Note the typo: this is in upstream LS, we preserve it.
    #[serde(rename = "moveConnetionRows")]
    MoveConnetionRows {
        ids: Vec<String>,
    },
    ClearConnectionRows,
    SetInspector {
        inspector: serde_json::Value,
    },
    UpdateRuleButtons {
        buttons: serde_json::Value,
    },
    HighlightRuleForRows {
        rule_id: String,
        row_ids: Vec<String>,
    },
    TrafficEvents {
        events: Vec<TrafficEvent>,
    },
    SetTrafficData {
        data: serde_json::Value,
    },
    UpdateTrafficData {
        data: serde_json::Value,
    },
    SetRules {
        rules: Vec<serde_json::Value>,
    },
    UpdateRules {
        rules: Vec<serde_json::Value>,
    },
    SetBlocklists {
        blocklists: Vec<BlocklistSummary>,
    },
    SetBlocklistDetails {
        details: BlocklistSummary,
    },
    SetBlocklistEntries {
        subscription_id: String,
        entries: Vec<BlocklistEntry>,
    },
    SetBlocklistEntryLocation {
        subscription_id: String,
        host: String,
        line_number: u64,
    },
    SetBlocklistStatus {
        subscription_id: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_failure_reason: Option<String>,
    },
    SetProfiles {
        profiles: Vec<ProfileSummary>,
    },
    ProfileChanged {
        active_profile_id: Option<String>,
    },
    SetConnectionsStatus {
        status: ConnectionsStatus,
    },
    SetAboutInfo {
        info: AboutInfo,
    },
    DiagnosticsReport {
        checks: Vec<DiagnosticCheck>,
    },
    SetUndoStack {
        stack: Vec<serde_json::Value>,
    },
    LocalizationTable {
        table: serde_json::Value,
    },
    GlobalSettings {
        settings: serde_json::Value,
    },
    /// A `Deny` verdict's requested scope couldn't be honored and was
    /// silently narrowed to an exact-host match — see
    /// `translator::verdict::ScopeDegradation` and issue #14's security
    /// review FIX 2/HIGH. Unlike the other variants here, this is a
    /// Snitchwatch-specific protocol extension, not one of the 22
    /// `handleServerCommand` cases upstream LS's `app.js` knows about (see
    /// this enum's doc comment) — a client that doesn't recognize it can
    /// safely ignore it.
    ///
    /// Sent alongside (not instead of) the desktop `Notice::
    /// DenyScopeNarrowed`: the desktop notice alone is silent for a
    /// headless `bridge-cli`, an unattended GUI, or any session with no
    /// D-Bus notification server, which is exactly the defect FIX 2
    /// existed to close (round 2 of the security review, HIGH). `reason`
    /// is always [`crate::translator::verdict::ScopeDegradation::describe`]'s
    /// fixed text — never built from connection data, so it needs no
    /// display-boundary sanitization on the way out.
    DenyScopeNarrowed {
        row_id: String,
        reason: String,
    },
    /// Daemon-reported aggregate counters from `Statistics` on a `Ping` call
    /// (issue #19). The `Connection` proto carries no byte counters, so the
    /// Traffic tab is rebuilt around these daemon-side aggregates instead of
    /// a synthesized per-connection byte stream. Deliberately omits the
    /// `by_*` map fields (`by_proto`/`by_address`/`by_host`/`by_port`/
    /// `by_uid`/`by_executable`) — those are per-key breakdowns with no
    /// consumer yet, not needed for the tile-grid summary this drives. Also
    /// omits `dns_responses` for the same reason — no tile surfaces it; add
    /// it here if a future tile needs it. Like
    /// `DenyScopeNarrowed`, this is a Snitchwatch-specific protocol
    /// extension with no equivalent `handleServerCommand` case upstream.
    DaemonStatistics {
        daemon_version: String,
        uptime: u64,
        rules: u64,
        connections: u64,
        ignored: u64,
        accepted: u64,
        dropped: u64,
        rule_hits: u64,
        rule_misses: u64,
    },
}

/// Client → server messages. These come from the UI's `sendAction(type, payload)`
/// calls. The `action` discriminator is the type name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClientMessage {
    SetVerdict {
        row_id: String,
        /// "allow" or "deny" — named `verdict` to avoid colliding with the
        /// envelope's `action` discriminator. Verify against captured LS payload.
        verdict: VerdictAction,
        scope: VerdictScope,
        /// How long the resulting rule should live — see [`VerdictDuration`].
        /// Optional on the wire: legacy clients (the vendored `web/` frontend
        /// predates the duration selector) send `remember: bool` instead.
        /// Resolve the effective value via [`effective_verdict_duration`] —
        /// never read this field directly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<VerdictDuration>,
        /// Legacy pre-duration field ("remember this decision"): `true` maps
        /// to [`VerdictDuration::Always`], absent/`false` to `Once` — but only
        /// when `duration` itself is absent. Current clients never send it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<bool>,
    },
    AddRule {
        rule: serde_json::Value,
    },
    UpdateRule {
        rule_id: String,
        rule: serde_json::Value,
    },
    DeleteRule {
        rule_id: String,
    },
    GlobalSettings {
        settings: serde_json::Value,
    },
    SubscribeBlocklist {
        url: String,
    },
    UnsubscribeBlocklist {
        id: String,
    },
    CreateProfile {
        id: String,
        name: String,
        network_matchers: Vec<String>,
    },
    UpdateProfile {
        id: String,
        name: String,
        network_matchers: Vec<String>,
    },
    DeleteProfile {
        id: String,
    },
    ActivateProfile {
        id: String,
    },
    DeactivateProfile,
    AddProfileRule {
        profile_id: String,
        rule: ProfileRuleWire,
    },
    RemoveProfileRule {
        profile_id: String,
        rule_id: String,
    },
    Undo,
    Redo,
    /// Ask the bridge to re-broadcast full state snapshots (connections,
    /// blocklists, profiles). Sent by in-process feed consumers after a
    /// `broadcast::RecvError::Lagged` so a model that skipped delta messages
    /// can recover instead of staying silently stale. Rules are NOT included:
    /// the bridge holds no rule cache (`SetRules` originates upstream of it),
    /// so a lagged rules feed recovers on the daemon's next rule push.
    RequestSnapshot,
    /// Pause or resume interactive filtering (tray "Pause/Resume filtering").
    /// While paused, `opensnitchd`'s own `DefaultAction: deny` is left
    /// untouched — the bridge itself auto-resolves every `AskRule` as
    /// `Allow-Once` instead of prompting, so a genuine bridge outage still
    /// fails closed. See `grpc_server::UiService::ask_rule` and
    /// `docs/superpowers/plans/2026-07-12-tray-filter-off.md`.
    SetFilteringPaused {
        paused: bool,
    },
    RecheckDiagnostics,
}

/// Resolve [`ClientMessage::SetVerdict`]'s effective duration from the new
/// `duration` field and the legacy `remember` flag: an explicit duration
/// always wins; otherwise legacy `remember: true` means [`VerdictDuration::
/// Always`] (that's exactly what the pre-duration protocol expressed with it)
/// and anything else is a one-shot verdict.
pub fn effective_verdict_duration(
    duration: Option<VerdictDuration>,
    remember: Option<bool>,
) -> VerdictDuration {
    duration.unwrap_or(if remember.unwrap_or(false) {
        VerdictDuration::Always
    } else {
        VerdictDuration::Once
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VerdictAction {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictScope {
    /// Exact destination host only.
    ThisHost,
    /// Wildcard the leftmost label of the destination host.
    AnyHostOnDomain,
    /// Drop the host operator entirely.
    AnyHost,
}

/// How long a verdict's resulting rule should live, per the Little-Snitch-
/// parity duration selector on the pending-decision dialog ("This time" /
/// "For 5 minutes" / "Until quit" / "Forever").
///
/// Maps onto opensnitchd's native `Rule.duration` semantics
/// (`vendor/opensnitch/daemon/rule/rule.go`): the daemon defines three named
/// durations (`once`, `until restart`, `always`) plus arbitrary
/// Go-`time.ParseDuration`-compatible strings (e.g. `"5m"`) for auto-expiring
/// temporary rules (`vendor/opensnitch/daemon/rule/loader.go`'s
/// `scheduleTemporaryRule`). The mapping used by [`Self::daemon_duration_str`]:
///
/// | UI option        | Wire value      | Daemon `Rule.duration` |
/// |-------------------|-----------------|------------------------|
/// | This time         | `once`          | `"once"`               |
/// | For 5 minutes     | `five_minutes`  | `"5m"`                 |
/// | Until quit        | `until_restart` | `"until restart"`      |
/// | Forever           | `always`        | `"always"`             |
///
/// "Until quit" is documented to the user as "until the process exits", but
/// opensnitchd has no per-process rule lifetime — the closest native
/// equivalent is "until restart" (the rule survives until the daemon itself
/// restarts). This is the one lossy mapping in the table above; there is no
/// tighter daemon primitive to bind it to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictDuration {
    Once,
    FiveMinutes,
    UntilRestart,
    Always,
}

impl VerdictDuration {
    /// The exact string opensnitchd's `Rule.duration` field expects.
    pub fn daemon_duration_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::FiveMinutes => "5m",
            Self::UntilRestart => "until restart",
            Self::Always => "always",
        }
    }

    /// Whether this duration persists the rule beyond the current connection
    /// (i.e. anything other than a one-shot decision).
    pub fn remembers(self) -> bool {
        !matches!(self, Self::Once)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRow {
    pub id: String,
    pub process: String,
    pub process_path: Option<String>,
    pub dst_host: String,
    pub dst_ip: String,
    pub dst_port: u16,
    pub protocol: String,
    pub direction: String,
    /// `null` for pending rows, `"allow"` / `"deny"` / `"blocklist"` once decided.
    pub action: Option<String>,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub started_at_ms: i64,
    /// Name of the opensnitchd rule that decided this connection's `action`,
    /// if known. `None` while the row is pending (no rule has fired yet —
    /// that's exactly why the daemon asked). Once decided, this is either the
    /// synthetic once-off rule name the bridge handed back for an interactive
    /// verdict (see `translator::verdict::rule_name_for`), or the name of a
    /// pre-existing rule the daemon itself reports as having matched, via
    /// `Statistics.events[].rule.name` on a `Ping` call (see
    /// `translator::connection::event_to_row`). Additive field: old wire
    /// payloads without it deserialize with `None` via `#[serde(default)]`,
    /// and it is omitted from serialized JSON when absent so existing
    /// consumers (the web frontend) that don't know about it are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrafficEvent {
    pub timestamp_ms: i64,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionsStatus {
    Connected,
    Reconnecting,
    Disconnected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AboutInfo {
    pub snitchwatch_version: String,
    pub opensnitchd_version: String,
    pub ebpf_commit: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlocklistSummary {
    pub id: String,
    pub display_name: String,
    pub url: String,
    pub entry_count: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_iso8601: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlocklistEntry {
    pub host: String,
}

/// One rule override within a profile, as carried over the wire (mirrors
/// `snitchwatch_bridge::profiles::store::ProfileRule`, kept as a distinct
/// type here the same way `BlocklistSummary` is distinct from
/// `blocklists::store::Subscription` — the wire shape is the stable
/// contract, the store shape is free to evolve independently).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRuleWire {
    pub id: String,
    pub action: String,
    pub operand: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub network_matchers: Vec<String>,
    pub rules: Vec<ProfileRuleWire>,
    pub active: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_message_round_trips_via_json() {
        let msg = ServerMessage::InsertConnectionRows {
            rows: vec![ConnectionRow {
                id: "r1".to_string(),
                process: "firefox".to_string(),
                process_path: Some("/usr/bin/firefox".to_string()),
                dst_host: "github.com".to_string(),
                dst_ip: "140.82.121.4".to_string(),
                dst_port: 443,
                protocol: "tcp".to_string(),
                direction: "outgoing".to_string(),
                action: None,
                bytes_sent: 0,
                bytes_received: 0,
                started_at_ms: 1_700_000_000_000,
                matched_rule: None,
            }],
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""action":"insertConnectionRows""#));
        assert!(json.contains(r#""dstHost":"github.com""#));
        assert!(
            !json.contains("matchedRule"),
            "matchedRule must be omitted when None, so old web-frontend consumers are unaffected: {json}"
        );

        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn deny_scope_narrowed_round_trips_via_json() {
        // Issue #14 security review round 2, HIGH: this must be a real
        // wire-protocol message the WS client actually receives, not just a
        // desktop-notification side channel.
        let msg = ServerMessage::DenyScopeNarrowed {
            row_id: "ask-7".to_string(),
            reason: "the destination host has no subdomain that can be safely wildcarded below \
                     its public suffix"
                .to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""action":"denyScopeNarrowed""#));
        assert!(json.contains(r#""rowId":"ask-7""#));

        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn connection_row_carries_matched_rule_when_decided() {
        let row = ConnectionRow {
            id: "r1".to_string(),
            process: "firefox".to_string(),
            process_path: Some("/usr/bin/firefox".to_string()),
            dst_host: "github.com".to_string(),
            dst_ip: "140.82.121.4".to_string(),
            dst_port: 443,
            protocol: "tcp".to_string(),
            direction: "outgoing".to_string(),
            action: Some("allow".to_string()),
            bytes_sent: 0,
            bytes_received: 0,
            started_at_ms: 1_700_000_000_000,
            matched_rule: Some("899-firefox-allow-out.json".to_string()),
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["matchedRule"], "899-firefox-allow-out.json");

        let parsed: ConnectionRow = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, row);
    }

    #[test]
    fn connection_row_without_matched_rule_field_defaults_to_none() {
        // Simulates an old wire payload (or a hand-authored test fixture)
        // that predates this field entirely.
        let json = serde_json::json!({
            "id": "r1",
            "process": "firefox",
            "processPath": null,
            "dstHost": "github.com",
            "dstIp": "140.82.121.4",
            "dstPort": 443,
            "protocol": "tcp",
            "direction": "outgoing",
            "action": null,
            "bytesSent": 0,
            "bytesReceived": 0,
            "startedAtMs": 0
        });
        let parsed: ConnectionRow = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.matched_rule, None);
    }

    #[test]
    fn move_connection_rows_preserves_upstream_typo() {
        let msg = ServerMessage::MoveConnetionRows {
            ids: vec!["r1".to_string()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains(r#""action":"moveConnetionRows""#),
            "must preserve upstream LS typo: {}",
            json
        );
    }

    #[test]
    fn client_set_verdict_parses() {
        let json = r#"{
            "action": "setVerdict",
            "rowId": "r1",
            "verdict": "allow",
            "scope": "this_host",
            "duration": "always"
        }"#;
        let parsed: ClientMessage = serde_json::from_str(json).unwrap();
        match parsed {
            ClientMessage::SetVerdict {
                row_id,
                verdict,
                scope,
                duration,
                remember,
            } => {
                assert_eq!(row_id, "r1");
                assert_eq!(verdict, VerdictAction::Allow);
                assert_eq!(scope, VerdictScope::ThisHost);
                assert_eq!(duration, Some(VerdictDuration::Always));
                assert_eq!(remember, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_set_verdict_parses_legacy_remember_shape() {
        // The pre-duration wire shape the vendored web/ frontend still sends.
        let json = r#"{
            "action": "setVerdict",
            "rowId": "r1",
            "verdict": "deny",
            "scope": "this_host",
            "remember": true
        }"#;
        let parsed: ClientMessage = serde_json::from_str(json).unwrap();
        match parsed {
            ClientMessage::SetVerdict {
                duration, remember, ..
            } => {
                assert_eq!(duration, None);
                assert_eq!(remember, Some(true));
                assert_eq!(
                    effective_verdict_duration(duration, remember),
                    VerdictDuration::Always
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn effective_verdict_duration_folds_legacy_and_new() {
        use VerdictDuration::*;
        // Explicit duration always wins, even over remember.
        assert_eq!(
            effective_verdict_duration(Some(FiveMinutes), Some(true)),
            FiveMinutes
        );
        // Legacy remember semantics.
        assert_eq!(effective_verdict_duration(None, Some(true)), Always);
        assert_eq!(effective_verdict_duration(None, Some(false)), Once);
        // Neither present: the safe default.
        assert_eq!(effective_verdict_duration(None, None), Once);
    }

    #[test]
    fn verdict_duration_maps_to_daemon_strings() {
        assert_eq!(VerdictDuration::Once.daemon_duration_str(), "once");
        assert_eq!(VerdictDuration::FiveMinutes.daemon_duration_str(), "5m");
        assert_eq!(
            VerdictDuration::UntilRestart.daemon_duration_str(),
            "until restart"
        );
        assert_eq!(VerdictDuration::Always.daemon_duration_str(), "always");

        assert!(!VerdictDuration::Once.remembers());
        assert!(VerdictDuration::FiveMinutes.remembers());
        assert!(VerdictDuration::UntilRestart.remembers());
        assert!(VerdictDuration::Always.remembers());
    }

    #[test]
    fn verdict_duration_wire_tokens_are_snake_case() {
        assert_eq!(
            serde_json::to_value(VerdictDuration::FiveMinutes).unwrap(),
            "five_minutes"
        );
        assert_eq!(
            serde_json::to_value(VerdictDuration::UntilRestart).unwrap(),
            "until_restart"
        );
    }
}

#[cfg(test)]
mod blocklist_message_tests {
    use super::*;

    #[test]
    fn set_blocklists_serializes_to_camel_case_action() {
        let msg = ServerMessage::SetBlocklists {
            blocklists: vec![BlocklistSummary {
                id: "stevenblack".into(),
                display_name: "StevenBlack".into(),
                url: "https://x.example/hosts".into(),
                entry_count: 1234,
                status: "ok".into(),
                last_updated_iso8601: Some("2026-04-11T12:00:00Z".into()),
                last_failure_reason: None,
            }],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setBlocklists");
        assert_eq!(json["blocklists"][0]["id"], "stevenblack");
        assert_eq!(json["blocklists"][0]["displayName"], "StevenBlack");
        assert_eq!(json["blocklists"][0]["entryCount"], 1234);
    }

    #[test]
    fn set_blocklist_entries_carries_strongly_typed_entries() {
        let msg = ServerMessage::SetBlocklistEntries {
            subscription_id: "stevenblack".into(),
            entries: vec![
                BlocklistEntry {
                    host: "doubleclick.net".into(),
                },
                BlocklistEntry {
                    host: "google-analytics.com".into(),
                },
            ],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setBlocklistEntries");
        assert_eq!(json["subscriptionId"], "stevenblack");
        assert_eq!(json["entries"][0]["host"], "doubleclick.net");
    }

    #[test]
    fn subscribe_blocklist_action_round_trips() {
        let action = ClientMessage::SubscribeBlocklist {
            url: "https://x.example/hosts".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, action);
    }
}

#[cfg(test)]
mod profile_message_tests {
    use super::*;

    fn summary(id: &str, active: bool) -> ProfileSummary {
        ProfileSummary {
            id: id.into(),
            name: "At Home".into(),
            network_matchers: vec!["Home*".into()],
            rules: vec![ProfileRuleWire {
                id: "r1".into(),
                action: "allow".into(),
                operand: "dest.host".into(),
                data: "nas.local".into(),
            }],
            active,
        }
    }

    #[test]
    fn set_profiles_serializes_to_camel_case_action() {
        let msg = ServerMessage::SetProfiles {
            profiles: vec![summary("home", true)],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setProfiles");
        assert_eq!(json["profiles"][0]["id"], "home");
        assert_eq!(json["profiles"][0]["networkMatchers"][0], "Home*");
        assert_eq!(json["profiles"][0]["rules"][0]["operand"], "dest.host");
        assert_eq!(json["profiles"][0]["active"], true);
    }

    #[test]
    fn set_profiles_round_trips() {
        let msg = ServerMessage::SetProfiles {
            profiles: vec![summary("home", false)],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn profile_changed_round_trips_with_and_without_active_id() {
        let with_id = ServerMessage::ProfileChanged {
            active_profile_id: Some("home".into()),
        };
        let json = serde_json::to_string(&with_id).unwrap();
        assert!(json.contains(r#""action":"profileChanged""#));
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            with_id
        );

        let none = ServerMessage::ProfileChanged {
            active_profile_id: None,
        };
        let json = serde_json::to_string(&none).unwrap();
        assert_eq!(serde_json::from_str::<ServerMessage>(&json).unwrap(), none);
    }

    #[test]
    fn create_profile_round_trips() {
        let action = ClientMessage::CreateProfile {
            id: "home".into(),
            name: "At Home".into(),
            network_matchers: vec!["Home*".into()],
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains(r#""action":"createProfile""#));
        assert!(json.contains(r#""networkMatchers""#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            action
        );
    }

    #[test]
    fn activate_and_deactivate_profile_round_trip() {
        let activate = ClientMessage::ActivateProfile { id: "home".into() };
        let json = serde_json::to_string(&activate).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            activate
        );

        let deactivate = ClientMessage::DeactivateProfile;
        let json = serde_json::to_string(&deactivate).unwrap();
        assert_eq!(json, r#"{"action":"deactivateProfile"}"#);
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            deactivate
        );
    }

    #[test]
    fn add_and_remove_profile_rule_round_trip() {
        let add = ClientMessage::AddProfileRule {
            profile_id: "home".into(),
            rule: ProfileRuleWire {
                id: "r1".into(),
                action: "deny".into(),
                operand: "dest.host".into(),
                data: "ads.example".into(),
            },
        };
        let json = serde_json::to_string(&add).unwrap();
        assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), add);

        let remove = ClientMessage::RemoveProfileRule {
            profile_id: "home".into(),
            rule_id: "r1".into(),
        };
        let json = serde_json::to_string(&remove).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            remove
        );
    }
}

#[cfg(test)]
mod filtering_pause_tests {
    use super::*;

    #[test]
    fn client_set_filtering_paused_round_trips() {
        let msg = ClientMessage::SetFilteringPaused { paused: true };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"action":"setFilteringPaused","paused":true}"#);
        assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn diagnostics_report_round_trips() {
        let msg = ServerMessage::DiagnosticsReport {
            checks: vec![
                DiagnosticCheck {
                    kind: CheckKind::DaemonReachable,
                    status: CheckStatus::Ok,
                },
                DiagnosticCheck {
                    kind: CheckKind::EbpfSupport,
                    status: CheckStatus::Failed {
                        detail: "no BTF".to_string(),
                    },
                },
                DiagnosticCheck {
                    kind: CheckKind::FirewallRunning,
                    status: CheckStatus::Unknown,
                },
            ],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"action\":\"diagnosticsReport\""));
        let round_tripped: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, msg);
    }

    #[test]
    fn recheck_diagnostics_round_trips() {
        let msg = ClientMessage::RecheckDiagnostics;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"action\":\"recheckDiagnostics\""));
        let round_tripped: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, msg);
    }
}

#[cfg(test)]
mod daemon_statistics_message_tests {
    use super::*;

    #[test]
    fn daemon_statistics_round_trips_via_json() {
        let msg = ServerMessage::DaemonStatistics {
            daemon_version: "1.8.0".to_string(),
            uptime: 3661,
            rules: 12,
            connections: 4200,
            ignored: 10,
            accepted: 4000,
            dropped: 200,
            rule_hits: 3900,
            rule_misses: 300,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "daemonStatistics");
        assert_eq!(json["daemonVersion"], "1.8.0");
        assert_eq!(json["uptime"], 3661);
        assert_eq!(json["rules"], 12);
        assert_eq!(json["connections"], 4200);
        assert_eq!(json["ignored"], 10);
        assert_eq!(json["accepted"], 4000);
        assert_eq!(json["dropped"], 200);
        assert_eq!(json["ruleHits"], 3900);
        assert_eq!(json["ruleMisses"], 300);

        let parsed: ServerMessage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, msg);
    }
}
