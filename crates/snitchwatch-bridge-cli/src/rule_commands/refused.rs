//! What a refused `CHANGE_RULE` (an add, an edit, a toggle) answers, by what
//! stock opensnitchd v1.8.0 left (`cache::rules_refused`; `Replace` stores
//! the rule, compiling it only if enabled, then `Save`s an `always` one):
//! - a disabled `always` rule applies as sent, not saved: `OkWithNote`, and
//!   the list shows it (#120 item 3, #153);
//! - an enabled `always` rule may or may not apply (compile error or `Save`
//!   failure): `Unsure`, never `Rejected` (every GUI reads that as "not
//!   changed") nor `OkWithNote` (read as done). The list stays as it was
//!   and nothing on a row records it (#120 item 7, owner decision);
//! - a temporary rule has no file, so only compiling failed: `Rejected`.

use snitchwatch_bridge::cache::rules::{applied_but_not_saved, may_have_applied};
use snitchwatch_bridge::ws_messages::RuleCommandOutcome;
use snitchwatch_proto::protocol::Notification;

/// A refused add's or edit's note: the daemon took the change, but its
/// saved file is the old version, or none.
pub(super) const CHANGED_NOT_SAVED: &str = "Changed for now. The firewall took the change, but \
     couldn't save it, so when the firewall restarts the rule goes back to how it was saved, or \
     is gone if it is new. If it does, make the change again.";

/// A refused change the daemon's answer can't place.
pub(super) const MAY_HAVE_APPLIED: &str = "Not confirmed. The firewall refused the change, but it \
     may or may not have applied: its answer doesn't say which, and the rule list may not show \
     it. If it matters, make the change again.";

/// What a refusal of a `CHANGE_RULE` means, decided before it is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IfRefused {
    Rejected,
    AppliedNotSaved,
    MayHaveApplied,
}

impl IfRefused {
    pub(super) fn of(sent: &Notification) -> Self {
        if applied_but_not_saved(sent) {
            Self::AppliedNotSaved
        } else if may_have_applied(sent) {
            Self::MayHaveApplied
        } else {
            Self::Rejected
        }
    }

    /// `outcome`, with a `Rejected` one answered as this refusal means;
    /// `not_saved` is the note for a change that applied, not saved.
    pub(super) fn answer(self, outcome: RuleCommandOutcome, not_saved: &str) -> RuleCommandOutcome {
        let RuleCommandOutcome::Rejected { reason } = outcome else {
            return outcome;
        };
        match self {
            Self::Rejected => RuleCommandOutcome::Rejected { reason },
            Self::AppliedNotSaved => RuleCommandOutcome::OkWithNote {
                note: format!("{not_saved} ({reason})"),
            },
            Self::MayHaveApplied => RuleCommandOutcome::Unsure {
                reason: format!("{MAY_HAVE_APPLIED} ({reason})"),
            },
        }
    }
}
