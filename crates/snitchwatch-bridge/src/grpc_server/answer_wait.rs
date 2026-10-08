//! How `ask_rule` waits for a pending prompt's answer: a verdict, the last
//! GUI session leaving, or nobody answering in time (prompt-slot plan Part
//! C, `crate::deferred_answers`).

use super::*;
use crate::client_presence::Admission;
use crate::deferred_answers::{answer_unanswered, NO_ANSWER};
use tokio::sync::oneshot;

/// Why a pending prompt got no verdict. Either way the daemon applies its
/// default action to the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unanswered {
    /// The last authenticated GUI session left.
    GuiLost,
    /// Nobody answered in time, "Decide later" put it off, or it was
    /// cancelled.
    NoAnswer,
}

impl Unanswered {
    pub(super) fn into_status(self) -> Status {
        match self {
            Self::GuiLost => Status::unavailable("last authenticated GUI session disconnected"),
            Self::NoAnswer => Status::unavailable(NO_ANSWER),
        }
    }
}

impl UiService {
    /// The verdict for pending row `row_id`. A verdict that settled first
    /// always wins over the loss and the timeout.
    pub(super) async fn wait_for_answer(
        &self,
        row_id: &str,
        admission: &mut Admission,
        mut verdict_rx: oneshot::Receiver<VerdictResolution>,
    ) -> Result<VerdictResolution, Unanswered> {
        tokio::select! {
            resolution = &mut verdict_rx => resolution,
            _ = admission.lost() => {
                if self.cache.lock().await.cancel_pending(row_id) {
                    return Err(Unanswered::GuiLost);
                }
                // A verdict serialized before the loss already settled this Ask.
                verdict_rx.await
            }
            _ = tokio::time::sleep(self.answer_timeout) => {
                if answer_unanswered(&self.cache, &self.daemon_config, &self.broadcast, row_id).await {
                    return Err(Unanswered::NoAnswer);
                }
                verdict_rx.await
            }
        }
        // The sender is gone without a verdict: put off by "Decide later"
        // (`deferred_answers::decide_later`) or cancelled.
        .map_err(|_| Unanswered::NoAnswer)
    }
}
