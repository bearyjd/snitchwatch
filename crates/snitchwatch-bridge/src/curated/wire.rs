//! What GUIs are told about each curated default.

use serde::{Deserialize, Serialize};

use super::reconcile::EntryStatus;

/// One entry, as the GUI lists it. Every text is the bridge's own: the
/// reviewed data file's, or fixed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CuratedDefaultSummary {
    pub id: String,
    /// The program's exact path.
    pub program: String,
    /// Exactly what the rule allows, in plain text.
    pub allows: String,
    /// Why it is offered.
    pub why: String,
    /// The user turned it on. Off unless they did.
    pub on: bool,
    pub status: EntryStatus,
    /// Why the last command for it failed, in fixed text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}
