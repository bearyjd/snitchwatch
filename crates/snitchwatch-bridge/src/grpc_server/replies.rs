//! How `ask_rule` answers and what an answer sets off: the verdict reply,
//! the paused allow on arrival and the tray's recent-block overlay (moved
//! out of `grpc_server.rs`).

use super::*;

impl UiService {
    /// The `AskRule` reply for a resolved verdict.
    ///
    /// A one-shot reply is deliberately absent from Rules. Every remembered
    /// rule is an active daemon rule, including a five-minute or
    /// until-restart rule, and must be visible/editable immediately rather
    /// than waiting for a daemon-side rule-list push that may never come.
    /// May diverge on the daemon's `setUniqueName`; see `cache::rules`.
    ///
    /// Issue #44: a remembered verdict `verdict_to_rule` refuses (no absolute
    /// process path) is answered once instead, never cached or announced as
    /// a rule, and every client is told why.
    pub(super) fn verdict_reply(
        &self,
        resolution: VerdictResolution,
        conn: &Connection,
        row_id: String,
        ask_id: u64,
        now_secs: i64,
    ) -> Rule {
        let refusal = match verdict_to_rule(
            resolution.verdict,
            resolution.duration,
            resolution.scope,
            conn,
            now_secs,
        ) {
            Ok(rule) => {
                if resolution.duration.remembers() {
                    self.rules.upsert(rule.clone());
                    if self.broadcast.receiver_count() > 0 {
                        if let Err(e) = self.broadcast.send(ServerMessage::UpdateRules {
                            rules: vec![rule_to_wire(&rule)],
                        }) {
                            warn!(error = %e, "persistent verdict rule broadcast failed");
                        }
                    }
                }
                return rule;
            }
            Err(refusal) => refusal,
        };

        if self.broadcast.receiver_count() > 0 {
            if let Err(e) = self.broadcast.send(ServerMessage::VerdictNotRemembered {
                row_id,
                reason: refusal.describe().to_string(),
            }) {
                warn!(error = %e, "verdict-not-remembered broadcast send failed");
            }
        }
        self.notice_bus
            .send(crate::notice::Notice::VerdictNotRemembered { row_id: ask_id });
        once_rule(resolution.verdict, resolution.scope, conn, now_secs)
    }

    /// Record an Ask the pause decides on arrival (issue #78): the row is
    /// stored already allowed and labelled `filterPaused`, and GUIs get it as
    /// a decided row, never as a waiting prompt. The caller holds the cache
    /// lock the pause's scan of waiting prompts takes too, and builds the
    /// Allow once reply after releasing it.
    pub(super) fn allow_on_arrival(&self, cache: &mut ConnectionCache, row: ConnectionRow) {
        let decided_row = crate::pause_answers::allowed_on_arrival(row);
        cache.insert_decided(decided_row.clone());
        if self.broadcast.receiver_count() > 0 {
            let msg = ServerMessage::InsertConnectionRows {
                rows: vec![decided_row],
            };
            if let Err(e) = self.broadcast.send(msg) {
                warn!(error = %e, "ask_rule (paused): broadcast send failed");
            }
        }
    }

    /// Publish `TrayState::RecentBlock` and schedule its own revert after
    /// [`RECENT_BLOCK_TTL`]. If a second block happens before the first's
    /// timer fires, the first's timer becomes a no-op (its captured
    /// generation no longer matches) — the newer block's own timer owns the
    /// eventual revert, so the tray never flickers back to a stale display
    /// mid-block.
    ///
    /// Not while the daemon is down (issue #58): the overlay would cover
    /// `DaemonDown` for the whole TTL. The check and the publish happen under
    /// the cache lock, which is also what the daemon watchdog holds when it
    /// marks the daemon down and publishes, so neither can interleave.
    pub(super) async fn publish_recent_block(&self, what: String) {
        let generation = {
            let cache = self.cache.lock().await;
            if cache.tray_state() == TrayState::DaemonDown {
                return;
            }
            // Numbered in publish order, under the lock.
            let generation = self.block_generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.tray_pub.set(TrayState::RecentBlock {
                what,
                ttl: RECENT_BLOCK_TTL,
            });
            generation
        };

        let cache = self.cache.clone();
        let block_generation = self.block_generation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RECENT_BLOCK_TTL).await;
            if block_generation.load(Ordering::SeqCst) == generation {
                // Revert via the cache, which already holds the same
                // publisher and knows the actual current Idle/Pending(n)
                // state — not a hardcoded Idle.
                cache.lock().await.resync_tray_state();
            }
        });
    }
}
