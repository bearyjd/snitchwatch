//! Serde structs for the 22 LS WebSocket message types.
//!
//! All server-to-client messages share the same envelope: `{action: "...", ...}`.
//! We model this as a tagged enum for round-trip type safety.

use serde::{Deserialize, Serialize};

use crate::notice::Notice;
use crate::tray_state::TrayState;

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
    /// Explicit acknowledgement that the first WebSocket frame contained the
    /// current service token. This is sent directly to the just-authenticated
    /// client before any broadcast traffic, so external clients must not
    /// consider a socket usable merely because writing the token succeeded.
    ///
    /// This is an additive Snitchwatch extension. Legacy web clients ignore
    /// its unknown `action` exactly as they do the other native-shell
    /// extensions below.
    ///
    /// `capabilities` lists the optional features this bridge supports
    /// (`crate::bridge_capabilities`, e.g. `"appBoundRules"`). It is omitted
    /// when empty, and older bridges never send it, so a client must treat a
    /// missing list as empty and ignore strings it doesn't know. Clients that
    /// decode this as a unit variant (v0.1.1) still accept it: serde ignores
    /// the extra field.
    Authenticated {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
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
        /// Whether subscriptions survive a bridge restart (issue #45). `None`
        /// from an older bridge; GUIs treat that as not persistent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        storage: Option<StorageStatus>,
    },
    SetBlocklistDetails {
        details: BlocklistSummary,
    },
    /// How many blocklist rules Snitchwatch made are still in the firewall
    /// with nothing managing them (issue #73): this service has no state
    /// directory, is a per-user one, or can't read its saved subscriptions.
    /// Sent after every `SetBlocklists`; `0` clears the page's notice. The
    /// user can remove them with `RemoveLeftoverBlocklistRules`. Additive:
    /// older clients ignore it.
    SetBlocklistLeftovers {
        count: u32,
    },
    /// One page (at most [`BLOCKLIST_ENTRIES_PAGE_MAX`] hosts, starting at
    /// `offset`) of a subscription's `total` hosts, sent only in answer to
    /// `RequestBlocklistEntries` (issue #45: a whole list in one frame
    /// overflowed GUI clients).
    SetBlocklistEntries {
        subscription_id: String,
        entries: Vec<BlocklistEntry>,
        #[serde(default)]
        offset: u64,
        #[serde(default)]
        total: u64,
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
        /// Whether profiles and the active-profile choice survive a bridge
        /// restart (issue #46). `None` from an older bridge; GUIs treat that
        /// as not persistent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        storage: Option<StorageStatus>,
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
    /// A remembered verdict for `row_id` was answered for this connection
    /// only: the daemon reported no absolute executable path, so no rule
    /// could be bound to the program (issue #44, second half — see
    /// `translator::process_binding::RuleRefusal`). `reason` is always
    /// `RuleRefusal::describe`'s fixed sentence. A Snitchwatch-specific
    /// extension like `DenyScopeNarrowed`; sent alongside the desktop
    /// `Notice::VerdictNotRemembered`.
    VerdictNotRemembered {
        row_id: String,
        reason: String,
    },
    /// Who holds opensnitchd's single prompt slot: the oldest open prompt,
    /// how many are open, and how many times the daemon applied its default
    /// action meanwhile (`None` while unknown; retries count again, and the
    /// figure is a lower bound). Sent on every
    /// change and in the `RequestSnapshot` answer; a native-shell extension
    /// legacy clients ignore. See `crate::prompt_slot`.
    PromptSlot {
        holder: Option<crate::prompt_slot::PromptSlotHolder>,
        holders: u32,
        defaulted_at_least: Option<u64>,
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
    /// Current bridge-owned tray state. This additive extension lets native
    /// shells consume the separately managed bridge without recreating its
    /// in-process state publishers. Older WebSocket clients can ignore this
    /// unknown action.
    TrayState {
        state: TrayState,
    },
    /// A bridge-owned desktop notification event. It is forwarded over the
    /// authenticated WebSocket so external shells retain their established
    /// notification controller input without starting another bridge.
    Notice {
        notice: Notice,
    },
    /// Whether filtering is paused and when the pause ends on its own
    /// (issue #47). Sent on every pause change and in every snapshot, so a
    /// GUI that connects mid-pause learns the end time. An additive
    /// extension: the legacy web client ignores unknown actions, and the
    /// Kirigami shell skips frames it can't parse. A Kirigami build from
    /// before #47 does not: it drops the connection on any unknown action and
    /// reconnects, so it can't be paired with a bridge that sends this.
    FilterPauseState {
        paused: bool,
        #[serde(default)]
        expires_at_unix_ms: Option<u64>,
    },
    /// Rule import/export (roadmap P2.7, `crate::rule_io`). Additive, like
    /// every extension above. Sent only to the requesting connection (an
    /// in-process sender gets them on the broadcast); `request_id` echoes
    /// the request's, and an apply's progress and result carry its
    /// `preview_id`. The answer to `ExportRules`:
    RulesExport {
        request_id: String,
        document: crate::rule_io::Document,
        omitted: crate::rule_io::OmittedCounts,
    },
    RulesExportUnavailable {
        request_id: String,
        reason: String,
    },
    /// The answer to `PreviewRulesImport`: one item per rule in the file.
    RulesImportPreview {
        request_id: String,
        preview_id: String,
        items: Vec<crate::rule_io::ImportItem>,
    },
    /// A preview or an apply was refused as a whole (fixed text).
    RulesImportRefused {
        request_id: String,
        reason: String,
    },
    /// One rule's outcome during `ApplyRulesImport`.
    RulesImportProgress {
        preview_id: String,
        name: String,
        outcome: crate::rule_io::ImportOutcome,
    },
    /// Sent once an apply ends, however it ends.
    RulesImportResult {
        preview_id: String,
        applied: u32,
        rejected: u32,
        not_sent: u32,
        no_answer: u32,
    },
    /// How often each daemon rule decided a connection, as Snitchwatch
    /// counted from the `events` in the daemon's pings (P2.6 Part 1, see
    /// `crate::cache::rule_hits`). Sent at most every 5 seconds and only
    /// when something changed, and in every `RequestSnapshot` answer, even
    /// before the first ping. An additive extension legacy clients ignore.
    ///
    /// The counts are **approximate and can be incomplete**: the daemon
    /// reports at most `Stats.MaxEvents` matched connections per ping and
    /// nothing while no bridge is connected, and a `nolog` rule never
    /// reports any. `since_unix_ms` is when counting began (`None` until the
    /// first ping that carried statistics); `last_gap_unix_ms` is when the
    /// bridge last noticed it may have missed events, and `lossy` is true
    /// once it has (the two are never out of step). `storage` says whether
    /// the counts survive a bridge restart.
    RuleHits {
        since_unix_ms: Option<i64>,
        lossy: bool,
        last_gap_unix_ms: Option<i64>,
        storage: StorageStatus,
        hits: Vec<RuleHitWire>,
    },
}

/// One rule's count in [`ServerMessage::RuleHits`], and in the saved hit
/// counts file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleHitWire {
    /// The daemon's rule name.
    pub name: String,
    pub count: u64,
    /// When the rule last decided a connection, in Unix milliseconds.
    pub last_hit_unix_ms: i64,
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
    /// Delete the leftover blocklist rules `SetBlocklistLeftovers` counts.
    /// Ignored while this service manages its blocklist rules.
    RemoveLeftoverBlocklistRules,
    /// Ask for a page of a subscription's hosts; answered with
    /// `SetBlocklistEntries`. `limit` is capped at
    /// [`BLOCKLIST_ENTRIES_PAGE_MAX`].
    RequestBlocklistEntries {
        subscription_id: String,
        #[serde(default)]
        offset: u64,
        #[serde(default)]
        limit: Option<u32>,
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
    /// blocklists, profiles, rules). Sent by in-process feed consumers after a
    /// `broadcast::RecvError::Lagged` so a model that skipped delta messages
    /// can recover instead of staying silently stale. Rules are included as
    /// `SetRules` once a daemon's rule snapshot has been committed during this
    /// bridge run (`cache::rules`); before that the bridge has no rule list
    /// to send.
    RequestSnapshot,
    /// Pause or resume interactive filtering (tray "Pause/Resume filtering").
    /// While paused, `opensnitchd`'s own `DefaultAction: deny` is left
    /// untouched — the bridge itself auto-resolves every `AskRule` as
    /// `Allow-Once` instead of prompting, so a genuine bridge outage still
    /// fails closed. See `grpc_server::UiService::ask_rule` and
    /// `docs/superpowers/plans/2026-07-12-tray-filter-off.md`.
    ///
    /// A pause is always timed (issue #47): `duration_secs` must be one of
    /// `filter_pause::ALLOWED_PAUSE_SECS`, and a pause without it (from an
    /// older client) lasts `filter_pause::LEGACY_PAUSE_SECS`.
    SetFilteringPaused {
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_secs: Option<u64>,
        /// The sender's GUI-session generation, stamped by `ws_server` on
        /// every request from an authenticated WebSocket session (see
        /// `client_presence::apply_pause_request`). Never on the wire: a
        /// client cannot supply it. `None` for in-process senders.
        #[serde(skip)]
        sender_generation: Option<u64>,
        /// The sender's uid (`SO_PEERCRED`), stamped alongside
        /// `sender_generation` so the outcome log names who paused. Never on
        /// the wire either.
        #[serde(skip)]
        sender_uid: Option<u32>,
    },
    RecheckDiagnostics,
    /// "Decide later" on a pending row (prompt-slot plan Part C): the bridge
    /// blocks the program for 5 minutes, or gives the daemon no answer when
    /// it can't name the program. Only for bridges advertising
    /// `bridge_capabilities::DECIDE_LATER`.
    DecideLater {
        row_id: String,
    },
    /// Rule import/export (roadmap P2.7); handled by bridge-cli's
    /// `rules_import` task, never by `upstream::apply`. `request_id` is the
    /// client's, echoed in the answer; `reply` is stamped by `ws_server`
    /// with the sending connection and never comes from the wire.
    ExportRules {
        #[serde(default)]
        request_id: String,
        #[serde(skip)]
        reply: Option<ReplyTo>,
    },
    /// Validate a rules document and preview it against the daemon's rules.
    /// Bounded by the client message cap (`ws_server`).
    PreviewRulesImport {
        #[serde(default)]
        request_id: String,
        document: serde_json::Value,
        #[serde(skip)]
        reply: Option<ReplyTo>,
    },
    /// Apply the named rules of the pending preview (`CHANGE_RULE` only).
    ApplyRulesImport {
        #[serde(default)]
        request_id: String,
        preview_id: String,
        include: Vec<String>,
        #[serde(skip)]
        reply: Option<ReplyTo>,
    },
}

/// A channel back to one WebSocket connection, stamped on rule import and
/// export requests by `ws_server` so their answers reach only the GUI that
/// asked. Never serialized.
#[derive(Clone)]
pub struct ReplyTo(pub tokio::sync::mpsc::Sender<ServerMessage>);

impl ReplyTo {
    /// Deliver `message`, waiting for room; `false` when the connection is
    /// gone.
    pub async fn send(&self, message: ServerMessage) -> bool {
        self.0.send(message).await.is_ok()
    }
}

impl std::fmt::Debug for ReplyTo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplyTo")
    }
}

impl PartialEq for ReplyTo {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
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
    /// The download result (`pending` / `ok` / `failed`), not enforcement.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_iso8601: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
    /// One of the `ENFORCEMENT_*` values. Empty from an older bridge, which
    /// enforced nothing.
    #[serde(default)]
    pub enforcement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement_reason: Option<String>,
}

/// Most hosts in one `SetBlocklistEntries` page (~260 KiB of JSON at most).
pub const BLOCKLIST_ENTRIES_PAGE_MAX: u32 = 1000;

/// [`BlocklistSummary::enforcement`]: not downloaded or pushed yet.
pub const ENFORCEMENT_PENDING: &str = "pending";
/// [`BlocklistSummary::enforcement`]: the daemon accepted the list's rule,
/// or already held it unchanged. The daemon may still have loaded 0
/// entries, so GUIs say "Rule installed", never "Enforced".
pub const ENFORCEMENT_RULE_INSTALLED: &str = "rule_installed";
/// [`BlocklistSummary::enforcement`]: nothing blocks this list's hosts; see
/// `enforcement_reason`.
pub const ENFORCEMENT_NOT_ENFORCED: &str = "not_enforced";

/// Where the bridge keeps blocklist subscriptions (`SetBlocklists`) or
/// profiles (`SetProfiles`); each store reports its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatus {
    /// True only when the data is saved in a state directory and survives a
    /// restart.
    pub persistent: bool,
    /// Why storage is not persistent, when it was configured but unusable,
    /// or, with `unreadable`, why the saved subscriptions couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Blocklists only: the saved subscriptions couldn't be read (issue
    /// #45): Snitchwatch leaves the firewall's blocklist rules as they are
    /// and installs nothing. Its own state, apart from `persistent`. An
    /// unreadable profile store is reported as not persistent instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unreadable: bool,
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
mod tests;

#[cfg(test)]
mod blocklist_message_tests;

#[cfg(test)]
mod profile_message_tests;

#[cfg(test)]
mod filtering_pause_tests;

#[cfg(test)]
mod daemon_statistics_message_tests;
