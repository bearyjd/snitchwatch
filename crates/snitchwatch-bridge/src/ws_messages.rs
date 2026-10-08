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
    /// What the rule list (`SetRules`) leaves out (issue #61), sent after
    /// every one: `too_large` rules over the per-rule size limits, and
    /// `over_limit_total`, while there is no list, the number of rules in a
    /// daemon snapshot over the 10,000 Snitchwatch reads.
    RulesNotShown {
        too_large: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        over_limit_total: Option<u32>,
    },
    /// The outcome of an `AddRule`/`UpdateRule`/`DeleteRule` that carried a
    /// `request_id` (rule editor, P2.1), sent to the asking connection only.
    RuleCommandResult {
        request_id: String,
        outcome: RuleCommandOutcome,
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

/// What happened to a rule command (P2.1). Every reason is plain text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RuleCommandOutcome {
    /// The daemon answered OK (both steps, for a rename).
    Ok,
    /// Done, with something the user should know (a renamed rule's old
    /// file the daemon couldn't remove).
    OkWithNote { note: String },
    /// The daemon answered ERROR, or a rename was undone; why.
    Rejected { reason: String },
    /// The bridge didn't send it: the rule policy's problems.
    Refused {
        problems: Vec<crate::rule_policy::RuleProblem>,
    },
    /// No answer in time: it may or may not have been applied.
    Timeout,
    /// No firewall service connected; nothing was sent.
    NoDaemon,
    /// A rename whose outcome isn't known (see the reason).
    Unsure { reason: String },
}

/// Whether a client's request id is usable: 1 to 64 ASCII letters, digits
/// or `-`. Anything else is treated as absent.
pub fn valid_request_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
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
    /// `request_id` (P2.1, optional; see [`valid_request_id`]) asks for a
    /// [`ServerMessage::RuleCommandResult`]; `reply` is stamped by
    /// `ws_server` and never comes from the wire.
    AddRule {
        rule: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip)]
        reply: Option<ReplyTo>,
    },
    /// A `rule_id` other than `rule.name` renames (P2.1, E1).
    UpdateRule {
        rule_id: String,
        rule: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip)]
        reply: Option<ReplyTo>,
    },
    DeleteRule {
        rule_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip)]
        reply: Option<ReplyTo>,
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
    /// `SetRules` and `RulesNotShown`; the list is empty until a daemon's
    /// rule snapshot has been committed (`cache::rules`).
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
/// asked. Never serialized. Clones share the connection's "stopped reading"
/// mark, so a GUI that stops reading is waited on once, not per request.
#[derive(Clone)]
pub struct ReplyTo {
    tx: tokio::sync::mpsc::Sender<ServerMessage>,
    stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ReplyTo {
    /// One connection's channel (call once per connection, then clone).
    pub fn new(tx: tokio::sync::mpsc::Sender<ServerMessage>) -> Self {
        Self {
            tx,
            stalled: std::sync::Arc::default(),
        }
    }

    /// Deliver `message`, waiting for room; `false` when the connection is
    /// gone.
    pub async fn send(&self, message: ServerMessage) -> bool {
        self.tx.send(message).await.is_ok()
    }

    /// Deliver `message` only if there is room now.
    pub fn try_send(&self, message: ServerMessage) -> bool {
        self.tx.try_send(message).is_ok()
    }

    /// Whether the connection was found not reading its answers.
    pub fn stalled(&self) -> bool {
        self.stalled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn mark_stalled(&self) {
        self.stalled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for ReplyTo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplyTo")
    }
}

impl PartialEq for ReplyTo {
    fn eq(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
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
                auto_answer: None,
                answer_deadline_ms: None,
                deferred: false,
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
    fn authenticated_ack_is_a_backward_compatible_wire_extension() {
        let message = ServerMessage::Authenticated {
            capabilities: Vec::new(),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(json, r#"{"action":"authenticated"}"#);
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            message
        );
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
    fn shell_state_extensions_round_trip_via_json() {
        let tray = ServerMessage::TrayState {
            state: TrayState::Pending(3),
        };
        let notice = ServerMessage::Notice {
            notice: Notice::Pending {
                row_id: 42,
                process: "firefox".into(),
            },
        };

        for message in [tray, notice] {
            let json = serde_json::to_string(&message).unwrap();
            let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, message);
        }
    }

    #[test]
    fn legacy_web_dispatcher_ignores_shell_only_wire_extensions() {
        // The bundled legacy web client has an explicit default branch for
        // unknown server actions. Keep the extension discriminators absent
        // from its known-action switch: authenticated native shells consume
        // them, while old web clients continue their normal message loop.
        let legacy_dispatcher = include_str!("../../../web/js/app.js");
        assert!(legacy_dispatcher.contains("function handleServerCommand(messageArray)"));
        assert!(legacy_dispatcher
            .contains("default:\n          console.warn(\"Unknown msg from server\""));

        for message in [
            ServerMessage::Authenticated {
                capabilities: crate::bridge_capabilities::advertised(),
            },
            ServerMessage::TrayState {
                state: TrayState::Idle,
            },
            ServerMessage::Notice {
                notice: Notice::DaemonAway,
            },
            ServerMessage::FilterPauseState {
                paused: false,
                expires_at_unix_ms: None,
            },
            crate::prompt_slot::PromptSlot::default().message(),
            ServerMessage::RuleHits {
                since_unix_ms: None,
                lossy: false,
                last_gap_unix_ms: None,
                storage: StorageStatus {
                    persistent: false,
                    reason: None,
                    unreadable: false,
                },
                hits: Vec::new(),
            },
        ] {
            let action = serde_json::to_value(message).unwrap()["action"]
                .as_str()
                .unwrap()
                .to_owned();
            assert!(
                !legacy_dispatcher.contains(&format!("case \"{action}\"")),
                "legacy web client must treat {action} as an ignorable unknown action"
            );
        }
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
            auto_answer: None,
            answer_deadline_ms: None,
            deferred: false,
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
                enforcement: ENFORCEMENT_NOT_ENFORCED.into(),
                enforcement_reason: Some("no rule sink yet".into()),
            }],
            storage: Some(StorageStatus {
                unreadable: false,
                persistent: false,
                reason: Some("blocklist store: disk full".into()),
            }),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setBlocklists");
        assert_eq!(json["blocklists"][0]["id"], "stevenblack");
        assert_eq!(json["blocklists"][0]["displayName"], "StevenBlack");
        assert_eq!(json["blocklists"][0]["entryCount"], 1234);
        assert_eq!(json["blocklists"][0]["enforcement"], "not_enforced");
        assert_eq!(
            json["blocklists"][0]["enforcementReason"],
            "no rule sink yet"
        );
        assert_eq!(json["storage"]["persistent"], false);
        assert_eq!(json["storage"]["reason"], "blocklist store: disk full");
    }

    /// An older bridge sends neither `storage` nor the enforcement fields.
    #[test]
    fn set_blocklists_from_an_older_bridge_still_parses() {
        let json = r#"{"action":"setBlocklists","blocklists":[{"id":"a","displayName":"A",
            "url":"https://x.example/a","entryCount":1,"status":"ok"}]}"#;
        match serde_json::from_str::<ServerMessage>(json).unwrap() {
            ServerMessage::SetBlocklists {
                blocklists,
                storage,
            } => {
                assert_eq!(storage, None);
                assert_eq!(blocklists[0].enforcement, "");
                assert_eq!(blocklists[0].enforcement_reason, None);
            }
            other => panic!("expected SetBlocklists, got {other:?}"),
        }
    }

    #[test]
    fn set_blocklist_entries_carries_strongly_typed_entries() {
        let msg = ServerMessage::SetBlocklistEntries {
            offset: 0,
            total: 2,
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

    /// Issue #45 (S2): the largest possible entries page (every host at the
    /// 253-byte maximum) stays far below a GUI client's 16 MiB frame limit.
    #[test]
    fn the_largest_entries_page_fits_one_small_frame() {
        let host = format!("{}.example", "a".repeat(245));
        let msg = ServerMessage::SetBlocklistEntries {
            subscription_id: "x".repeat(81),
            entries: (0..BLOCKLIST_ENTRIES_PAGE_MAX)
                .map(|_| BlocklistEntry { host: host.clone() })
                .collect(),
            offset: u64::MAX,
            total: u64::MAX,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.len() < 1024 * 1024, "{} bytes", json.len());
    }

    #[test]
    fn request_blocklist_entries_parses_with_defaults() {
        let parsed: ClientMessage =
            serde_json::from_str(r#"{"action":"requestBlocklistEntries","subscriptionId":"a"}"#)
                .unwrap();
        assert_eq!(
            parsed,
            ClientMessage::RequestBlocklistEntries {
                subscription_id: "a".into(),
                offset: 0,
                limit: None,
            }
        );
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
            storage: Some(StorageStatus {
                unreadable: false,
                persistent: false,
                reason: Some("profile store: disk full".into()),
            }),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["action"], "setProfiles");
        assert_eq!(json["profiles"][0]["id"], "home");
        assert_eq!(json["profiles"][0]["networkMatchers"][0], "Home*");
        assert_eq!(json["profiles"][0]["rules"][0]["operand"], "dest.host");
        assert_eq!(json["profiles"][0]["active"], true);
        assert_eq!(json["storage"]["persistent"], false);
        assert_eq!(json["storage"]["reason"], "profile store: disk full");
    }

    /// Issue #46: an older bridge sends no `storage`; GUIs treat that as not
    /// persistent.
    #[test]
    fn set_profiles_from_an_older_bridge_still_parses() {
        let json = r#"{"action":"setProfiles","profiles":[{"id":"a","name":"A",
            "networkMatchers":[],"rules":[],"active":false}]}"#;
        match serde_json::from_str::<ServerMessage>(json).unwrap() {
            ServerMessage::SetProfiles { profiles, storage } => {
                assert_eq!(storage, None);
                assert_eq!(profiles[0].id, "a");
            }
            other => panic!("expected SetProfiles, got {other:?}"),
        }
    }

    #[test]
    fn set_profiles_round_trips() {
        let msg = ServerMessage::SetProfiles {
            profiles: vec![summary("home", false)],
            storage: Some(StorageStatus {
                unreadable: false,
                persistent: true,
                reason: None,
            }),
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

    fn set_filtering_paused(paused: bool, duration_secs: Option<u64>) -> ClientMessage {
        ClientMessage::SetFilteringPaused {
            paused,
            duration_secs,
            sender_generation: None,
            sender_uid: None,
        }
    }

    #[test]
    fn client_set_filtering_paused_round_trips() {
        let msg = set_filtering_paused(true, Some(1800));
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"action":"setFilteringPaused","paused":true,"durationSecs":1800}"#
        );
        assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), msg);

        let resume = set_filtering_paused(false, None);
        let json = serde_json::to_string(&resume).unwrap();
        assert_eq!(json, r#"{"action":"setFilteringPaused","paused":false}"#);
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).unwrap(),
            resume
        );
    }

    #[test]
    fn legacy_pause_without_a_duration_still_parses() {
        assert_eq!(
            serde_json::from_str::<ClientMessage>(
                r#"{"action":"setFilteringPaused","paused":true}"#
            )
            .unwrap(),
            set_filtering_paused(true, None)
        );
    }

    #[test]
    fn a_client_cannot_supply_the_sender_generation() {
        let parsed: ClientMessage = serde_json::from_str(
            r#"{"action":"setFilteringPaused","paused":true,"durationSecs":300,"senderGeneration":7,"senderUid":0}"#,
        )
        .unwrap();
        assert_eq!(parsed, set_filtering_paused(true, Some(300)));

        let stamped = ClientMessage::SetFilteringPaused {
            paused: true,
            duration_secs: Some(300),
            sender_generation: Some(7),
            sender_uid: Some(0),
        };
        let json = serde_json::to_string(&stamped).unwrap();
        assert!(
            !json.contains("sender"),
            "stamp leaked onto the wire: {json}"
        );
    }

    /// P2.6 Part 1: the largest `RuleHits` the bridge can send (every rule
    /// it tracks, each name at the length limit and made of characters JSON
    /// doubles, the widest numbers, a long storage reason) fits one frame of
    /// a GUI client with tungstenite's default limit (the Kirigami shell's
    /// `client_async` uses the default config).
    #[test]
    fn the_largest_rule_hits_fits_a_gui_clients_frame() {
        use crate::cache::rule_hits::{MAX_HIT_NAME_BYTES, MAX_TRACKED_RULES};
        let limit = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_frame_size
            .expect("tungstenite's default has a frame limit");
        let hits = (0..MAX_TRACKED_RULES)
            .map(|i| {
                let stem = format!("{i:05}");
                let fill = "\"\\".repeat((MAX_HIT_NAME_BYTES - stem.len()) / 2);
                RuleHitWire {
                    name: format!("{stem}{fill}"),
                    count: u64::MAX,
                    last_hit_unix_ms: i64::MIN,
                }
            })
            .collect();
        let msg = ServerMessage::RuleHits {
            since_unix_ms: Some(i64::MIN),
            lossy: true,
            last_gap_unix_ms: Some(i64::MIN),
            storage: StorageStatus {
                persistent: false,
                // Two paths at PATH_MAX and an OS error, all escaped.
                reason: Some("\"".repeat(16 * 1024)),
                unreadable: false,
            },
            hits,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.len() < limit, "{} bytes, limit {limit}", json.len());
    }

    #[test]
    fn rule_hits_round_trips_with_camel_case_keys() {
        let message = ServerMessage::RuleHits {
            since_unix_ms: Some(1_800_000_000_000),
            lossy: true,
            last_gap_unix_ms: Some(1_800_000_100_000),
            storage: StorageStatus {
                persistent: false,
                reason: Some("no state directory".into()),
                unreadable: false,
            },
            hits: vec![RuleHitWire {
                name: "000-allow-curl".into(),
                count: 7,
                last_hit_unix_ms: 1_800_000_050_000,
            }],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "action": "ruleHits",
                "sinceUnixMs": 1_800_000_000_000_i64,
                "lossy": true,
                "lastGapUnixMs": 1_800_000_100_000_i64,
                "storage": {"persistent": false, "reason": "no state directory"},
                "hits": [{"name": "000-allow-curl", "count": 7, "lastHitUnixMs": 1_800_000_050_000_i64}],
            })
        );
        assert_eq!(
            serde_json::from_value::<ServerMessage>(json).unwrap(),
            message
        );
    }

    #[test]
    fn filter_pause_state_round_trips() {
        let paused = ServerMessage::FilterPauseState {
            paused: true,
            expires_at_unix_ms: Some(1_800_000_300_000),
        };
        let json = serde_json::to_string(&paused).unwrap();
        assert_eq!(
            json,
            r#"{"action":"filterPauseState","paused":true,"expiresAtUnixMs":1800000300000}"#
        );
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            paused
        );

        let not_paused = ServerMessage::FilterPauseState {
            paused: false,
            expires_at_unix_ms: None,
        };
        let json = serde_json::to_string(&not_paused).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            not_paused
        );
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
