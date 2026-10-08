//! The guard that ends an import's apply however it ends (P2.7 review #7).

use snitchwatch_bridge::cache::rules::PublishHold;
use snitchwatch_bridge::ws_messages::ServerMessage;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::apply::Totals;
use crate::replier::Replier;

/// Owns a running apply's state. [`finish`](Self::finish) ends it; so does
/// dropping it (its task panicked or was cancelled): either way the held
/// rule list is published once, the import is freed for the next apply, and
/// a `RulesImportResult` with the totals so far is sent, waiting briefly for
/// room rather than being dropped.
pub(crate) struct ApplyRun {
    running: Arc<AtomicBool>,
    hold: Option<PublishHold>,
    replier: Replier,
    preview_id: String,
    pub(crate) totals: Totals,
    finished: bool,
}

impl ApplyRun {
    pub(crate) fn new(
        running: Arc<AtomicBool>,
        hold: PublishHold,
        replier: Replier,
        preview_id: String,
    ) -> Self {
        Self {
            running,
            hold: Some(hold),
            replier,
            preview_id,
            totals: Totals::default(),
            finished: false,
        }
    }

    /// Publish, free the import, and send the result.
    pub(crate) async fn finish(mut self) {
        let result = self.end();
        self.replier.send_final(result).await;
    }

    /// Publish the held rules and free the import; the result to send.
    fn end(&mut self) -> ServerMessage {
        self.finished = true;
        // One `SetRules` with every confirmed rule, before the result.
        drop(self.hold.take());
        self.running.store(false, Ordering::SeqCst);
        ServerMessage::RulesImportResult {
            preview_id: self.preview_id.clone(),
            applied: self.totals.applied,
            rejected: self.totals.rejected,
            not_sent: self.totals.not_sent,
            no_answer: self.totals.no_answer,
        }
    }
}

impl Drop for ApplyRun {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let result = self.end();
        // Ended abnormally: still deliver the result, off this stack.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let replier = self.replier.clone();
                runtime.spawn(async move { replier.send_final(result).await });
            }
            Err(_) => self.replier.send_now(result),
        }
    }
}
