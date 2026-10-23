//! Learning mode's "Apply N rules" (phase L4a): the sequencer. It sends a
//! batch of planned rules ONE `AddRule` at a time, each with a request id,
//! and waits for its `RuleCommandResult`. No Qt and no rendering here; a UI
//! reads the [`Progress`] value.
//!
//! * Before anything is sent it asks for the rules list and requires a READ
//!   one (`snapshot`): an empty list while the bridge has none is not "no
//!   such rule". No list, no batch.
//! * `AddRule` replaces a rule of the same name, so each name is looked up
//!   first. The same content is skipped; other content (the owner edited
//!   it) is left alone and reported, never overwritten (`compare`).
//! * A rule's outcome is shown per row. `Refused` and `Rejected` do not stop
//!   the batch; `NoDaemon` and three `Timeout`s in a row do. `Timeout` and
//!   `Unsure` are settled by looking at the list again and comparing
//!   content, never by sending again.
//! * A full queue is retried with a growing wait; Cancel leaves the rest
//!   "not sent".
//! * Names come from `learning_review::rules_for`; [`BulkPlan::new`] refuses
//!   any name in a family Snitchwatch keeps for itself, `snitchwatch-allow-`
//!   included.
//!
//! The sequencer is written against [`RuleSink`]; [`BridgeSink`] is the
//! adapter onto the bridge handles. `run` blocks, so call it from a worker
//! thread, never from the UI or the async runtime.

mod bridge_sink;
mod compare;
mod plan;
mod progress;
mod run;
mod sink;
mod snapshot;

pub use bridge_sink::BridgeSink;
pub use plan::{BulkPlan, PlanError, PlannedRule, LEGACY_ALLOW_PREFIX};
pub use progress::{AbortReason, BatchState, Counts, NotSentWhy, Progress, Row, RowStatus};
pub use run::run;
pub use sink::{Incoming, RuleSink, SendError, SessionLost, Settings};

#[cfg(test)]
mod tests;
