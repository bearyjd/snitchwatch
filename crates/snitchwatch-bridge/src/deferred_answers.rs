//! Prompt-slot plan Part C: a prompt nobody answers, and "Decide later".
//!
//! opensnitchd asks about one connection at a time, so a prompt nobody
//! answers holds every other new connection to the daemon's default action
//! until its 120 s `AskRule` deadline. Owner decisions S1 and S2:
//! - **No answer (S1 = P-a).** After [`ANSWER_TIMEOUT`] the waiting
//!   `AskRule` returns `Unavailable("no answer")`. The daemon applies its
//!   default action to that one connection and stores no rule
//!   (`vendor:daemon/main.go` `acceptOrDeny`). The row stays listed:
//!   deferred, labelled [`AutoAnswer::NoAnswer`], with that action when
//!   [`crate::daemon_config`] knows it.
//! - **Decide later (S2 = P-c).** Denies the program on any host for 5
//!   minutes, through the same `resolve` as a GUI's Deny, so its retries are
//!   dropped without asking; then it is asked about again. That needs a
//!   program file a rule can name (#44). Without one it is P-a, at once.
//!
//! Both settle under the cache lock like every verdict, so a racing answer
//! either wins or finds the prompt gone.

use std::time::Duration;

use tokio::sync::{broadcast, Mutex};

use crate::cache::connections::{CacheError, ConnectionCache, Verdict};
use crate::daemon_config::SharedDaemonConfig;
use crate::translator::process_binding::is_bindable_process_path;
use crate::ws_messages::{AutoAnswer, ConnectionRow, ServerMessage, VerdictDuration, VerdictScope};

/// How long a prompt waits for a person (S1: stock-UI parity).
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// The `AskRule` error message for a prompt nobody answered.
pub(crate) const NO_ANSWER: &str = "no answer";

/// `row`, pending from now, with the time the bridge answers it if nobody
/// does.
pub(crate) fn with_answer_deadline(row: ConnectionRow, timeout: Duration) -> ConnectionRow {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    ConnectionRow {
        answer_deadline_ms: Some(now_ms.saturating_add(timeout.as_millis() as i64)),
        ..row
    }
}

/// The timeout: put the prompt off and tell every GUI. False when a verdict
/// won first; the caller then takes that verdict.
pub(crate) async fn answer_unanswered(
    cache: &Mutex<ConnectionCache>,
    daemon_config: &SharedDaemonConfig,
    broadcast_tx: &broadcast::Sender<ServerMessage>,
    row_id: &str,
) -> bool {
    let mut cache = cache.lock().await;
    let action = daemon_config.default_row_action();
    let Some(row) = cache.defer_pending(row_id, action, Some(AutoAnswer::NoAnswer)) else {
        return false;
    };
    tracing::info!(%row_id, "nobody answered the prompt; the daemon applies its default action");
    let _ = broadcast_tx.send(ServerMessage::UpdateConnectionRows { rows: vec![row] });
    true
}

/// What "Decide later" did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecidedLater {
    /// The program is denied on any host for 5 minutes (P-c).
    BlockedForFiveMinutes,
    /// No program file to name, so the daemon got no answer (P-a).
    DefaultAction,
}

/// "Decide later" for pending row `row_id` (`ClientMessage::DecideLater`).
/// Sends the deferred row to every GUI. An error means the row isn't
/// waiting any more, and nothing changed.
pub async fn decide_later(
    cache: &Mutex<ConnectionCache>,
    daemon_config: &SharedDaemonConfig,
    broadcast_tx: &broadcast::Sender<ServerMessage>,
    row_id: &str,
) -> Result<DecidedLater, CacheError> {
    let mut cache = cache.lock().await;
    let bindable = cache
        .rows()
        .iter()
        .find(|row| row.id == row_id)
        .and_then(|row| row.process_path.as_deref())
        .is_some_and(is_bindable_process_path);
    let (outcome, row) = if bindable {
        cache.resolve(
            row_id,
            Verdict::Deny,
            VerdictDuration::FiveMinutes,
            VerdictScope::AnyHost,
        )?;
        (
            DecidedLater::BlockedForFiveMinutes,
            cache.mark_deferred(row_id),
        )
    } else {
        let action = daemon_config.default_row_action();
        let row = cache
            .defer_pending(row_id, action, None)
            .ok_or_else(|| CacheError::NotPending(row_id.to_owned()))?;
        (DecidedLater::DefaultAction, Some(row))
    };
    if let Some(row) = row {
        let _ = broadcast_tx.send(ServerMessage::UpdateConnectionRows { rows: vec![row] });
    }
    tracing::info!(%row_id, ?outcome, "prompt decided later");
    Ok(outcome)
}
