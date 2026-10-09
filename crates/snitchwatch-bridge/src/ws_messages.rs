//! Serde structs for the 22 LS WebSocket message types.
//!
//! All server-to-client messages share the same envelope: `{action: "...", ...}`.
//! We model this as a tagged enum for round-trip type safety.

use serde::{Deserialize, Serialize};

mod blocklist_wire;
mod verdict_wire;
pub use blocklist_wire::{
    BlocklistEntry, BlocklistSummary, BLOCKLIST_ENTRIES_PAGE_MAX, ENFORCEMENT_NOT_ENFORCED,
    ENFORCEMENT_PENDING, ENFORCEMENT_RULE_INSTALLED, LEFTOVER_CAUSE_NO_STATE_DIR,
    LEFTOVER_CAUSE_STORE_UNREADABLE,
};

pub use verdict_wire::{
    effective_verdict_duration, AutoAnswer, ConnectionRow, VerdictAction, VerdictDuration,
    VerdictScope,
};

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
    /// older clients ignore it. `cause` says why nothing manages them (one of
    /// the `LEFTOVER_CAUSE_*` values; a client treats one it doesn't know
    /// like none), because with an unreadable store they are probably lists
    /// the user still subscribes to. `reason` is how the last removal went
    /// when it didn't fully succeed: plain text, absent otherwise.
    SetBlocklistLeftovers {
        count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// One page (at most [`BLOCKLIST_ENTRIES_PAGE_MAX`] hosts, starting at
    /// `offset`) of a subscription's `total` hosts, sent only in answer to
    /// `RequestBlocklistEntries` (issue #45: a whole list in one frame
    /// overflowed GUI clients). Sent to every connected GUI, so
    /// `request_id` echoes the request it answers (issue #67): a GUI keeps
    /// only its own pages. `last_updated_iso8601` is when the list was last
    /// downloaded: pages of one list with different values come from
    /// different contents, and a GUI starts over rather than mix them.
    SetBlocklistEntries {
        subscription_id: String,
        entries: Vec<BlocklistEntry>,
        #[serde(default)]
        offset: u64,
        #[serde(default)]
        total: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_updated_iso8601: Option<String>,
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
        /// Whether this bridge installs the active profile's rules (issue
        /// #46 Part 2). `false` from an older bridge, which never did.
        #[serde(default)]
        applies_rules: bool,
        /// Why it doesn't, when it doesn't (per-user mode, no saved state).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        not_applied_reason: Option<String>,
    },
    ProfileChanged {
        active_profile_id: Option<String>,
    },
    /// The curated default rules for background services and where each
    /// stands (prompt-slot plan Part D; `bridge_capabilities::CURATED_DEFAULTS`).
    SetCuratedDefaults {
        entries: Vec<crate::curated::wire::CuratedDefaultSummary>,
        storage: StorageStatus,
        /// Why this bridge never installs them (the per-user bridge, or no
        /// saved settings); `None` when it does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unavailable: Option<String>,
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
        /// Whether the bridge has the daemon's rule list: a GUI says it is
        /// waiting for one rather than "no rules" (PR #106 review).
        #[serde(default)]
        listed: bool,
        /// Deleted rules the daemon stopped using but couldn't remove the
        /// saved file of: each may come back when it restarts (tower r12,
        /// `RulesCache::files_left`). Omitted when 0.
        #[serde(default, skip_serializing_if = "is_zero")]
        left_on_disk: u32,
        /// The daemon has kept reporting a different number of rules than
        /// the list holds, for several pings in a row (issue #65, option c;
        /// `RulesCache::count_mismatch`). Advice only: the list is not
        /// re-sent. Omitted when false; an older GUI ignores it, and an
        /// older bridge never sends it.
        #[serde(default, skip_serializing_if = "is_false")]
        count_mismatch: bool,
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

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_false(flag: &bool) -> bool {
    !*flag
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
    /// Delete the leftover blocklist rules `SetBlocklistLeftovers` counts.
    /// Ignored while this service manages its blocklist rules.
    RemoveLeftoverBlocklistRules,
    /// Ask for a page of a subscription's hosts; answered with
    /// `SetBlocklistEntries`. `limit` is capped at
    /// [`BLOCKLIST_ENTRIES_PAGE_MAX`]. `request_id` (optional; see
    /// [`valid_request_id`]) comes back on the page that answers it. The
    /// page goes only to the connection that asked (`reply`, stamped by
    /// `ws_server`, never from the wire); a request with no connection (an
    /// in-process sender) is answered on the broadcast.
    RequestBlocklistEntries {
        subscription_id: String,
        #[serde(default)]
        offset: u64,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip)]
        reply: Option<ReplyTo>,
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
    /// `request_id` (optional; see [`valid_request_id`]) asks for a
    /// [`ServerMessage::RuleCommandResult`]: refused with the profile
    /// policy's problems, or ok (saved to the profile, not yet installed).
    AddProfileRule {
        profile_id: String,
        rule: ProfileRuleWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip)]
        reply: Option<ReplyTo>,
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
    /// Turn the curated defaults `ids` on or off (prompt-slot plan Part D).
    /// Unknown ids are ignored.
    SetCuratedDefaults {
        ids: Vec<String>,
        on: bool,
    },
    /// Remove an entry's rule that was edited outside Snitchwatch, after the
    /// user confirmed (prompt-slot D, code review M2). Only that entry's own
    /// reserved name is ever deleted; an unknown id does nothing.
    RemoveCuratedDefault {
        id: String,
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

    /// The connection took an answer again: it is reading after all.
    pub fn clear_stalled(&self) {
        self.stalled
            .store(false, std::sync::atomic::Ordering::SeqCst);
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
    /// The saved data couldn't be read, so Snitchwatch leaves the firewall's
    /// rules for it as they are and installs nothing: blocklists (issue
    /// #45) and the recommended rules' choices (prompt-slot D). Its own
    /// state, apart from `persistent`. An unreadable profile store is
    /// reported as not persistent instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unreadable: bool,
}

/// One rule override within a profile, as carried over the wire (mirrors
/// `snitchwatch_bridge::profiles::store::ProfileRule`, kept as a distinct
/// type here the same way `BlocklistSummary` is distinct from
/// `blocklists::store::Subscription` — the wire shape is the stable
/// contract, the store shape is free to evolve independently).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRuleWire {
    pub id: String,
    pub action: String,
    /// Part 1's single condition (empty when `operator` is set).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub operand: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
    /// The rule editor's conditions in #48's wire shape (issue #46 Part 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<serde_json::Value>,
    /// Bridge to GUI, while the rule's profile is active: one of the
    /// `ENFORCEMENT_*` values ("rule_installed" only after the daemon's
    /// correlated OK). Empty for a profile that isn't active, and from an
    /// older bridge.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub enforcement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement_reason: Option<String>,
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
