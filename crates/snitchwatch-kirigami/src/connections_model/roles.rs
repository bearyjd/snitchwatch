//! Role ids exposed to the QML delegate.

// Role ids exposed to the QML delegate. Flat-mode leaf rows use 0-6; the
// grouping layer (Little-Snitch-parity Process->Domain view) adds 7+. None
// of these names collide with Qt Quick Controls built-ins on `ItemDelegate`
// (in particular, never name a role `action` — see module docs on
// `connections::grouping` / the design spec's pitfall note: a role or
// required-property named `action` silently breaks delegates).
pub(super) const ROLE_ID: i32 = 0;
pub(super) const ROLE_PROCESS: i32 = 1;
pub(super) const ROLE_HOST: i32 = 2;
pub(super) const ROLE_PORT: i32 = 3;
pub(super) const ROLE_PROTOCOL: i32 = 4;
pub(super) const ROLE_VERDICT: i32 = 5;
pub(super) const ROLE_PENDING: i32 = 6;
pub(super) const ROLE_DEPTH: i32 = 7;
pub(super) const ROLE_IS_GROUP_HEADER: i32 = 8;
pub(super) const ROLE_EXPANDED: i32 = 9;
pub(super) const ROLE_GROUP_KEY: i32 = 10;
pub(super) const ROLE_GROUP_PARENT_KEY: i32 = 11;
pub(super) const ROLE_GROUP_LABEL: i32 = 12;
pub(super) const ROLE_GROUP_TOTAL: i32 = 13;
pub(super) const ROLE_GROUP_PENDING: i32 = 14;
pub(super) const ROLE_GROUP_ALLOWED: i32 = 15;
pub(super) const ROLE_GROUP_DENIED: i32 = 16;
pub(super) const ROLE_GROUP_BLOCKLISTED: i32 = 17;
/// Raw matched-rule name (empty string when unknown/not applicable), used by
/// QML to drive the inspector's "Show rule" action — see
/// `ConnectionsPage.qml`'s inspector and `RulesModel::select_rule_by_name`.
pub(super) const ROLE_MATCHED_RULE: i32 = 18;
/// Friendly display string for the same — see
/// `connections::row_store::matched_rule_display`'s doc comment for exactly
/// what it shows for pending / no-rule-name rows.
pub(super) const ROLE_MATCHED_RULE_DISPLAY: i32 = 19;
pub(super) const ROLE_SOURCE_SESSION: i32 = 20;
/// The bridge allowed this row once because filtering was paused (#78).
pub(super) const ROLE_ANSWERED_WHILE_PAUSED: i32 = 21;
/// A deferred row's verdict label (`connections::outcome::outcome_text`),
/// or a default-decided row's (E3); empty otherwise. Prompt-slot plan Part C.
pub(super) const ROLE_OUTCOME_TEXT: i32 = 22;
/// When the bridge answers a pending row itself, in Unix ms; -1 if never.
/// A `real`: epoch milliseconds overflow a QML `int`.
pub(super) const ROLE_ANSWER_DEADLINE_MS: i32 = 23;
/// "Make a rule…" is offered (`make_rule::offers_make_rule`): the prompt was
/// put off, or the firewall's default action decided the row (E3).
pub(super) const ROLE_MAKE_RULE_OFFERED: i32 = 24;
/// Executable path of the process, empty when the daemon did not report one.
pub(super) const ROLE_PROCESS_PATH: i32 = 25;
/// `"outgoing"` / `"incoming"` as the bridge reports it; empty on headers.
pub(super) const ROLE_DIRECTION: i32 = 26;
/// When the connection started, in Unix ms (a `real`: ms overflow a QML
/// `int`); -1 on headers.
pub(super) const ROLE_STARTED_AT_MS: i32 = 27;
/// Icon name (or path) for the process, from installed `.desktop` files;
/// the generic executable icon when none matches, empty on headers.
pub(super) const ROLE_APP_ICON: i32 = 28;
/// Whether Snitchwatch answered the row silently (once-only) and it still
/// has no rule (`crate::silent_mode`); false on headers.
pub(super) const ROLE_NEEDS_RULE: i32 = 29;
/// The tree tier: 0 program, 1 domain, 2 host, 3 connection (every row in
/// flat mode is a connection).
pub(super) const ROLE_GROUP_LEVEL: i32 = 30;
/// The owning program's group key on host headers (what `toggleHostGroup`
/// needs besides the domain and host keys); empty elsewhere.
pub(super) const ROLE_GROUP_PROCESS_KEY: i32 = 31;
/// What a tree header's rule button shows (`connections::level_rules::RuleState::token`):
/// `none`, `allow`, `deny`, `partial`, `pending` or `unavailable`; `none` on
/// connection rows.
pub(super) const ROLE_LEVEL_RULE_STATE: i32 = 32;
/// The name of the rule a delete would remove, only while the state is
/// `allow`, `deny` or `pending`; empty otherwise.
pub(super) const ROLE_LEVEL_RULE_NAME: i32 = 33;
/// Why the state is `unavailable` (fixed plain text); empty otherwise.
pub(super) const ROLE_LEVEL_RULE_REASON: i32 = 34;
/// How many daemon events a connection row stands for (issue #55): 1 unless
/// repeats collapsed into it; 0 on headers.
pub(super) const ROLE_REPEAT_COUNT: i32 = 35;
/// When a collapsed row's flow was last seen (epoch ms as a `real`); its own
/// start when nothing collapsed into it, -1 on headers.
pub(super) const ROLE_LAST_SEEN_MS: i32 = 36;
/// Friendly program label (`crate::app_names`); on connection rows and program
/// headers the sanitized name or heuristic/desktop label, on domain and host
/// headers their own label.
pub(super) const ROLE_PROGRAM_LABEL: i32 = 37;
/// Whether `programLabel` came from a forgeable source or was altered, so the
/// UI shows the unverified marker and the real path. False on domain/host headers.
pub(super) const ROLE_PROGRAM_LABEL_UNVERIFIED: i32 = 38;
/// Short tail of the real executable path, only while `programLabelUnverified`;
/// empty otherwise.
pub(super) const ROLE_PROGRAM_PATH_TAIL: i32 = 39;
/// The full real path with control, bidi and format characters shown as
/// visible escapes (`program_label::path_full_display`), for the tooltip;
/// empty when there is no path.
pub(super) const ROLE_PROGRAM_PATH_FULL: i32 = 40;
/// How many desktop notices of the row's flow were held back within the
/// hour (0d part 2, `connections_model::retries`): only on the flow's newest
/// pending row, 0 elsewhere and on headers.
pub(super) const ROLE_RETRIED_COUNT: i32 = 41;
/// A header's verdict counts for the last hour / today (phase 0b,
/// `connections::grouping::windows`): a JSON list of `{kind, text}`, one per
/// verdict present; `[]` on connection rows.
pub(super) const ROLE_GROUP_LAST_HOUR: i32 = 42;
pub(super) const ROLE_GROUP_TODAY: i32 = 43;
