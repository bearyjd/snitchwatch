//! Issue #44, second half: which process paths a remembered rule may be bound
//! to, and why the bridge declines to remember an answer otherwise.
//!
//! The daemon fills `Connection.process_path` from `/proc/<pid>/exe` when it
//! can. When it can't, it falls back to the placeholder `"Kernel connection"`
//! or to the process-chosen `comm` / argv\[0\]
//! (`vendor/opensnitch/daemon/procmon/details.go` `ReadPath`). A rule bound
//! to such a value matches any process the daemon describes the same way, and
//! any local user can name a binary to collide with a comm name. So only an
//! **absolute** path is a program identity worth remembering (owner decision,
//! `docs/superpowers/plans/2026-10-07-app-bound-prompt-scopes-part2.md`).

use snitchwatch_proto::protocol::Connection;

/// Whether `process_path` is an executable path a remembered rule may be bound
/// to. The single source of this rule for the bridge (`verdict_to_rule`,
/// `rule_name_for`) and for GUIs deciding which durations to offer.
pub fn is_bindable_process_path(process_path: &str) -> bool {
    process_path.starts_with('/')
}

/// `conn.process_path` when [`is_bindable_process_path`] accepts it.
pub fn bindable_process_path(conn: &Connection) -> Option<&str> {
    let path = conn.process_path.as_str();
    is_bindable_process_path(path).then_some(path)
}

/// Why the bridge answered a remembered verdict for this connection only.
///
/// Like `ScopeDegradation`, it carries no connection data: [`Self::describe`]
/// is a fixed sentence, safe for notification bodies and protocol text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleRefusal {
    /// The connection has no absolute `process_path` (see the module doc).
    ProcessFileUnknown,
}

impl RuleRefusal {
    /// A fixed, user-facing explanation — never built from connection data.
    pub fn describe(self) -> &'static str {
        match self {
            Self::ProcessFileUnknown => {
                "Snitchwatch couldn't identify this program's file, so this answer applies only \
                 to this connection."
            }
        }
    }
}
