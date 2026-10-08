//! Issue #78: a filtering pause also answers the prompts already waiting.
//!
//! When a pause takes effect, every `AskRule` the bridge is still holding
//! for the pausing GUI session is answered Allow once, through
//! [`ConnectionCache::resolve`], the call a GUI's Allow once goes through
//! too. A once answer is never saved as a rule. An Ask that arrives during
//! the pause is allowed once on arrival by `ask_rule`. Both kinds of row are
//! labelled [`AutoAnswer::FilterPaused`].
//!
//! No waiting prompt is missed: [`answer_waiting`] scans under the cache lock
//! after the pause is set, and `ask_rule` chooses between prompting and
//! allowing under the same lock. Either the scan sees the pending row, or
//! the row's insertion sees the pause.

use crate::cache::connections::{ConnectionCache, Verdict};
use crate::filter_pause::FilterPause;
use crate::ws_messages::{AutoAnswer, ConnectionRow, ServerMessage, VerdictDuration, VerdictScope};
use tokio::sync::{broadcast, Mutex};

/// Answer Allow once every waiting prompt the pause applies to (see
/// [`FilterPause::applies_to`]: admitted under the GUI-session generation
/// that set the pause, which is still current), and label it. Returns the
/// answered rows for the caller to send to GUIs.
pub(crate) fn allow_waiting(
    cache: &mut ConnectionCache,
    pause: &FilterPause,
) -> Vec<ConnectionRow> {
    cache
        .pending_admitted_where(|admission| pause.applies_to(admission))
        .into_iter()
        .filter_map(|row_id| {
            if let Err(error) = cache.resolve(
                &row_id,
                Verdict::Allow,
                VerdictDuration::Once,
                VerdictScope::ThisHost,
            ) {
                tracing::info!(%row_id, %error, "waiting prompt not answered by the pause");
                return None;
            }
            cache.label_auto_answer(&row_id, AutoAnswer::FilterPaused)
        })
        .collect()
}

/// [`allow_waiting`] under the cache lock, then send the answered rows to
/// every GUI. The bridge CLI's pump calls this after every pause request,
/// before it announces the pause state. Returns how many were answered.
pub async fn answer_waiting(
    pause: &FilterPause,
    cache: &Mutex<ConnectionCache>,
    broadcast_tx: &broadcast::Sender<ServerMessage>,
) -> usize {
    let mut cache = cache.lock().await;
    let rows = allow_waiting(&mut cache, pause);
    let answered = rows.len();
    if answered > 0 {
        tracing::info!(answered, "filtering pause allowed the waiting prompts once");
        let _ = broadcast_tx.send(ServerMessage::UpdateConnectionRows { rows });
    }
    answered
}

/// The decided row for an Ask that arrives while the pause applies.
pub(crate) fn allowed_on_arrival(row: ConnectionRow) -> ConnectionRow {
    ConnectionRow {
        action: Some("allow".to_string()),
        auto_answer: Some(AutoAnswer::FilterPaused),
        ..row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_is_camel_case_on_the_wire_and_an_unknown_one_still_parses() {
        assert_eq!(
            serde_json::to_value(AutoAnswer::FilterPaused).unwrap(),
            "filterPaused"
        );
        let row: ConnectionRow = serde_json::from_value(serde_json::json!({
            "id": "1", "process": "curl", "processPath": null, "dstHost": "a",
            "dstIp": "", "dstPort": 443, "protocol": "tcp",
            "direction": "outgoing", "action": "allow", "bytesSent": 0,
            "bytesReceived": 0, "startedAtMs": 0,
            "autoAnswer": "someReasonFromANewerBridge"
        }))
        .expect("a newer bridge's reason must not break the row");
        assert_eq!(row.auto_answer, Some(AutoAnswer::Unknown));
    }
}
