//! How `ask_rule` waits for a pending prompt's answer: a verdict, the last
//! GUI session leaving, or nobody answering in time (prompt-slot plan Part
//! C, `crate::deferred_answers`).

use super::*;
use crate::cache::connections::{PendingVerdict, UnansweredVerdict};
use crate::client_presence::Admission;
use crate::deferred_answers::{answer_unanswered, NO_ANSWER};
use tokio::sync::oneshot;

/// Why a pending prompt got no verdict. Either way the daemon applies its
/// default action to the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unanswered {
    /// The last authenticated GUI session left.
    GuiLost,
    /// The cache settled the prompt without a verdict.
    Settled(UnansweredVerdict),
    /// The sender closed without a verdict or an elapsed deadline.
    Closed,
}

impl Unanswered {
    pub(super) fn into_status(self) -> Status {
        match self {
            Self::GuiLost => Status::unavailable("last authenticated GUI session disconnected"),
            Self::Settled(_) | Self::Closed => Status::unavailable(NO_ANSWER),
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
        mut verdict_rx: oneshot::Receiver<PendingVerdict>,
    ) -> Result<VerdictResolution, Unanswered> {
        let outcome = tokio::select! {
            resolution = &mut verdict_rx => resolution,
            _ = admission.lost() => {
                if self.cache.lock().await.cancel_pending(row_id) {
                    return Err(Unanswered::GuiLost);
                }
                // A verdict serialized before the loss already settled this Ask.
                verdict_rx.await
            }
            _ = tokio::time::sleep(self.answer_timeout) => {
                {
                    let mut cache = self.cache.lock().await;
                    if admission.while_current(|| {
                        answer_unanswered(&mut cache, &self.daemon_config, &self.broadcast, row_id);
                    }).is_none() && cache.cancel_pending(row_id) {
                        return Err(Unanswered::GuiLost);
                    }
                }
                verdict_rx.await
            }
        };
        outcome
            .map_err(|_| Unanswered::Closed)?
            .map_err(Unanswered::Settled)
    }
}
