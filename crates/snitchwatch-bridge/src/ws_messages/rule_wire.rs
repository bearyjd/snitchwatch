//! Rule-command outcome and rule-hit wire types, re-exported from
//! `ws_messages`.

use serde::{Deserialize, Serialize};

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
    /// A command whose outcome isn't known (see the reason): a rename, or
    /// a refused change the daemon may or may not have applied (#120 item
    /// 7). Neither done nor not done.
    Unsure { reason: String },
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
