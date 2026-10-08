//! The wire types of verdicts and connection rows, re-exported from
//! `ws_messages`.

use serde::{Deserialize, Serialize};

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
/// parity duration selector on the Kirigami shell's pending-decision dialog
/// ("This time" / "For 5 minutes" / "Until firewall restarts" / "Forever").
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
/// | Until firewall restarts | `until_restart` | `"until restart"` |
/// | Forever           | `always`        | `"always"`             |
///
/// The third option used to read "Until quit", though opensnitchd has no
/// per-process rule lifetime: "until restart" keeps the rule until the daemon
/// itself restarts, so Kirigami now labels it for that (its QML token is still
/// `until_quit`). The Tauri shell and the vendored web UI keep their labels as
/// they are: neither offers a duration selector (they send the legacy
/// `remember` instead).
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
    /// A `deferred` row may also be `null`: the daemon applied its default
    /// action and the bridge doesn't know which one that is.
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
    /// Set when the bridge answered this connection itself rather than a
    /// person (issue #78). Additive and omitted when absent, like
    /// `matched_rule`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_answer: Option<AutoAnswer>,
    /// When the bridge answers this pending row itself if nobody does
    /// (prompt-slot plan Part C), in Unix milliseconds. Only on pending rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_deadline_ms: Option<i64>,
    /// The prompt was put off rather than decided: nobody answered it in
    /// time, or someone chose "Decide later". A rule can still be made for
    /// it. Additive, omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
}

/// Why the bridge answered a connection without a person (issue #78).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AutoAnswer {
    /// Allowed once because filtering was paused: either the prompt was
    /// already waiting when the pause took effect, or the connection arrived
    /// during it (`pause_answers`).
    FilterPaused,
    /// Nobody answered within `deferred_answers::ANSWER_TIMEOUT`, so the
    /// daemon applied its default action (prompt-slot plan Part C).
    NoAnswer,
    /// A reason this build doesn't know, from a newer bridge. Keeps the row
    /// readable instead of failing the whole message.
    #[serde(other)]
    Unknown,
}
