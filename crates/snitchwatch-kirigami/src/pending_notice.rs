//! A waiting prompt's desktop notification, from showing it to acting on
//! the answer (prompt-slot plan Part B; Allow follows the Settings duration
//! since the LS redesign, Deny is remembered until the firewall restarts).
//!
//! Runs on the bridge runtime. The notification is sent and heard through
//! `crate::notification_signals`, so only the notification server's click
//! counts. The answer goes through `notification_actions::act_and_confirm`.
//! The notification is resident, so the server keeps it after a click: it
//! is closed here once it is clicked, once its row stops waiting, or once
//! the server that showed it goes (its buttons are then void).

use std::time::Duration;

use crate::bridge_runtime::{BridgeHandles, BridgeNotice, PendingRow};
use crate::inline_deny::{allow_label, InlineDeny};
use crate::notification_actions::{
    act_and_confirm, pending_body, still_waiting, NoticeAction, ALLOW_ONCE_ACTION, DENY_ACTION,
    REVIEW_ACTION,
};
use crate::notification_signals::{Notice, WaitEnd};

const SUMMARY: &str = "Snitchwatch — pending decision";
/// The action keys, in the order the notification shows them.
const KEYS: [&str; 3] = [ALLOW_ONCE_ACTION, DENY_ACTION, REVIEW_ACTION];
/// How often a shown notice checks that its row still waits.
const ROW_CHECK: Duration = Duration::from_secs(1);

/// A pending notice's row, while it still waits in the notice's session.
pub(crate) struct PendingTarget {
    handles: BridgeHandles,
    connection_id: u64,
    wire_id: String,
    row: PendingRow,
    /// The Settings duration an Allow remembers for, read once when the
    /// notice is shown: its label and its click use the same one.
    allow_duration: &'static str,
}

impl PendingTarget {
    pub(crate) fn of(
        handles: &BridgeHandles,
        connection_id: u64,
        notice: &BridgeNotice,
    ) -> Option<Self> {
        let (wire_id, row) = still_waiting(handles, connection_id, notice)?;
        Some(Self {
            handles: handles.clone(),
            connection_id,
            wire_id,
            row,
            allow_duration: crate::settings::normalize_duration(""),
        })
    }

    /// The flow this prompt is about, for the notice cooldown: the
    /// program's path (its name when the daemon sent none) and the host.
    pub(crate) fn flow_key(&self) -> crate::notifier::FlowKey {
        crate::notifier::FlowKey {
            session: self.connection_id,
            program: self
                .row
                .process_path
                .clone()
                .unwrap_or_else(|| self.row.process.clone()),
            host: self.row.dst_host.clone(),
        }
    }

    pub(crate) fn handles(&self) -> &BridgeHandles {
        &self.handles
    }
}

/// Show `target`'s notification on `conn` and act on its answer.
/// `on_review` raises the window.
pub(crate) async fn show_and_answer(
    conn: &zbus::Connection,
    mut target: PendingTarget,
    on_review: impl FnOnce(),
) {
    let deny = InlineDeny::decide(
        target.row.process_path.as_deref(),
        target
            .handles
            .advertises_app_bound_rules(target.connection_id),
    );
    let body = pending_body(&target.row, deny);
    // A file read: off the runtime's worker, and once, for label and click.
    target.allow_duration = tokio::task::spawn_blocking(|| {
        crate::settings::read_settings(&crate::paths::settings_path()).duration_token()
    })
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, "settings read failed; the default duration is used");
        crate::settings::normalize_duration("")
    });
    let allow = allow_label(deny, target.allow_duration);
    let actions = [
        (ALLOW_ONCE_ACTION, allow.as_str()),
        (DENY_ACTION, "Deny"),
        (REVIEW_ACTION, "Review"),
    ];
    let mut notice = match Notice::show(conn, SUMMARY, &body, &actions).await {
        Ok(notice) => notice,
        Err(error) => {
            tracing::warn!(%error, "pending notification not shown");
            return;
        }
    };
    tracing::info!(
        session = target.connection_id,
        row = %target.wire_id,
        "pending notification shown for a waiting prompt"
    );
    match notice.wait(&KEYS, row_stops_waiting(&target)).await {
        WaitEnd::Action(key) => {
            // Resident: the server keeps it after the click, so it is
            // closed here, alongside the answer rather than before it: a
            // slow `CloseNotification` mustn't delay the answer (PR #112
            // review L2), nor the answer the close.
            tokio::join!(on_action(&target, key, on_review), notice.close());
        }
        // A new server voids the notice's buttons, so it goes; the row
        // still waits, to be answered in the window.
        WaitEnd::Stopped | WaitEnd::ServerChanged => notice.close().await,
        WaitEnd::Closed(_) => {}
    }
}

async fn row_stops_waiting(target: &PendingTarget) {
    while target
        .handles
        .pending_row(target.connection_id, &target.wire_id)
        .is_some()
    {
        tokio::time::sleep(ROW_CHECK).await;
    }
}

async fn on_action(target: &PendingTarget, key: &str, on_review: impl FnOnce()) {
    tracing::info!(
        session = target.connection_id,
        row = %target.wire_id,
        key,
        "pending notification clicked"
    );
    if key == REVIEW_ACTION {
        on_review();
        return;
    }
    let Some(action) = NoticeAction::from_id(key) else {
        return;
    };
    let outcome = act_and_confirm(
        &target.handles,
        target.connection_id,
        &target.wire_id,
        action,
        target.allow_duration,
    )
    .await;
    tracing::info!(
        session = target.connection_id,
        row = %target.wire_id,
        ?action,
        ?outcome,
        "answer from the pending notification"
    );
    if let Some(text) = outcome.explanation() {
        let shown =
            tokio::task::spawn_blocking(move || show_plain("Snitchwatch — your answer", text));
        if let Err(error) = shown.await {
            tracing::warn!(%error, "follow-up notification task failed");
        }
    }
}

/// A notification with fixed text and no actions.
pub(crate) fn show_plain(summary: &str, body: &str) {
    if let Err(err) = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .icon("security-high")
        .show()
    {
        tracing::warn!(?err, "failed to dispatch desktop notification");
    }
}
