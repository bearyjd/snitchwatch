//! Rule import and export for the bridge's GUIs (roadmap P2.7).
//!
//! One task owns the state: at most one pending preview (a new preview
//! replaces it; it expires after [`ImportConfig::preview_ttl`]) and whether
//! an apply is running. The inbound pump only routes the three messages
//! here ([`RulesImport::try_route`]) and never waits on them; parsing and
//! the rule policy (which compiles regexps) run on the blocking pool, off
//! the cache lock.
//!
//! An apply is refused when its preview is unknown or expired, when the
//! rules cache's revision moved since the preview (any toggle, delete,
//! prompt rule, expiry or reconnect), or while another apply runs. Every
//! `ServerMessage` is broadcast to every GUI; any `snitchwatch-ui` member can
//! already change rules, so a preview has no per-session owner. Import only
//! ever sends `CHANGE_RULE` ([`apply`]); it never deletes a rule.

use snitchwatch_bridge::auth::Token;
use snitchwatch_bridge::cache::rules::{RulesCache, SharedRulesCache};
use snitchwatch_bridge::daemon_commands::DaemonCommands;
use snitchwatch_bridge::rule_io::{self, ExportUnavailable, PreviewUnavailable};
use snitchwatch_bridge::ws_messages::{ClientMessage, ServerMessage};
use snitchwatch_proto::protocol::Rule;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

mod apply;

pub(crate) const UNKNOWN_PREVIEW: &str =
    "This import preview is no longer available. Preview the file again.";
pub(crate) const STALE_PREVIEW: &str = "Rules changed since the preview. Preview the file again.";
pub(crate) const IMPORT_RUNNING: &str =
    "Another import is still being applied. Wait for it to finish.";
const QUEUE_FULL: &str = "Snitchwatch is still working on earlier import requests. Try again.";
const INTERNAL: &str = "The rules couldn't be read because of an internal error.";

/// Import or export requests waiting for the task.
const REQUEST_QUEUE: usize = 8;

pub(crate) struct ImportConfig {
    /// How long each rule's daemon answer is awaited (#48's pump uses 5 s).
    pub(crate) reply_timeout: Duration,
    /// Pause before re-sending a command no stream could queue.
    pub(crate) retry_delay: Duration,
    /// How long a preview may be applied.
    pub(crate) preview_ttl: Duration,
}

impl Default for ImportConfig {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(5),
            retry_delay: Duration::from_millis(100),
            preview_ttl: Duration::from_secs(10 * 60),
        }
    }
}

/// The pump's handle to the import task.
pub(crate) struct RulesImport {
    tx: mpsc::Sender<ClientMessage>,
    broadcast: broadcast::Sender<ServerMessage>,
}

impl RulesImport {
    pub(crate) fn spawn(
        commands: DaemonCommands,
        rules: SharedRulesCache,
        broadcast: broadcast::Sender<ServerMessage>,
    ) -> Self {
        Self::spawn_with(commands, rules, broadcast, ImportConfig::default())
    }

    pub(crate) fn spawn_with(
        commands: DaemonCommands,
        rules: SharedRulesCache,
        broadcast: broadcast::Sender<ServerMessage>,
        config: ImportConfig,
    ) -> Self {
        let (tx, rx) = mpsc::channel(REQUEST_QUEUE);
        let task = ImportTask {
            commands,
            rules,
            broadcast: broadcast.clone(),
            config,
            pending: None,
            running: Arc::default(),
        };
        tokio::spawn(task.run(rx));
        Self { tx, broadcast }
    }

    /// Queue an import or export message for the task (`None`), or hand any
    /// other message back. Never waits.
    pub(crate) fn try_route(&self, msg: ClientMessage) -> Option<ClientMessage> {
        let is_export = matches!(msg, ClientMessage::ExportRules);
        if !is_export
            && !matches!(
                msg,
                ClientMessage::PreviewRulesImport { .. } | ClientMessage::ApplyRulesImport { .. }
            )
        {
            return Some(msg);
        }
        if self.tx.try_send(msg).is_err() {
            warn!("rule import queue full; request dropped");
            let reason = QUEUE_FULL.to_string();
            let _ = self.broadcast.send(if is_export {
                ServerMessage::RulesExportUnavailable { reason }
            } else {
                ServerMessage::RulesImportRefused { reason }
            });
        }
        None
    }
}

struct PendingPreview {
    id: String,
    created: Instant,
    revision: u64,
    /// The previewed `Add`/`Replace` rules, by name.
    rules: BTreeMap<String, Rule>,
}

struct ImportTask {
    commands: DaemonCommands,
    rules: SharedRulesCache,
    broadcast: broadcast::Sender<ServerMessage>,
    config: ImportConfig,
    pending: Option<PendingPreview>,
    running: Arc<AtomicBool>,
}

fn lock(cache: &SharedRulesCache) -> MutexGuard<'_, RulesCache> {
    cache.lock().unwrap_or_else(|e| e.into_inner())
}

impl ImportTask {
    async fn run(mut self, mut rx: mpsc::Receiver<ClientMessage>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::ExportRules => self.export().await,
                ClientMessage::PreviewRulesImport { document } => self.preview(document).await,
                ClientMessage::ApplyRulesImport {
                    preview_id,
                    include,
                } => self.apply(&preview_id, include),
                _ => warn!("rule import task got a message it doesn't handle"),
            }
        }
    }

    fn send(&self, message: ServerMessage) {
        let _ = self.broadcast.send(message);
    }

    fn refuse(&self, reason: &str) {
        self.send(ServerMessage::RulesImportRefused {
            reason: reason.to_string(),
        });
    }

    async fn export(&self) {
        // A copy, so the policy runs without holding the cache lock.
        let snapshot = lock(&self.rules).clone();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let exported =
            tokio::task::spawn_blocking(move || rule_io::export(&snapshot, now_ms)).await;
        self.send(match exported {
            Ok(Ok(export)) => {
                info!(rules = export.document.rules.len(), "exported rules");
                ServerMessage::RulesExport {
                    document: export.document,
                    omitted: export.omitted,
                }
            }
            Ok(Err(ExportUnavailable)) => ServerMessage::RulesExportUnavailable {
                reason: ExportUnavailable::REASON.to_string(),
            },
            Err(join_error) => {
                error!(error = %join_error, "rule export failed");
                ServerMessage::RulesExportUnavailable {
                    reason: INTERNAL.to_string(),
                }
            }
        });
    }

    async fn preview(&mut self, document: serde_json::Value) {
        self.pending = None;
        let checked = tokio::task::spawn_blocking(move || {
            rule_io::parse_document(document).map(|doc| rule_io::check_rules(&doc))
        })
        .await;
        let checked = match checked {
            Ok(Ok(checked)) => checked,
            Ok(Err(document_error)) => return self.refuse(document_error.describe()),
            Err(join_error) => {
                error!(error = %join_error, "rule import preview failed");
                return self.refuse(INTERNAL);
            }
        };
        let (classified, revision) = {
            let cache = lock(&self.rules);
            (rule_io::classify(&checked, &cache), cache.revision())
        };
        let preview = match classified {
            Ok(preview) => preview,
            Err(PreviewUnavailable) => return self.refuse(PreviewUnavailable::REASON),
        };
        let id = Token::generate().as_str()[..16].to_string();
        info!(rules = preview.items.len(), "previewed a rule import");
        self.pending = Some(PendingPreview {
            id: id.clone(),
            created: Instant::now(),
            revision,
            rules: preview.applicable,
        });
        self.send(ServerMessage::RulesImportPreview {
            preview_id: id,
            items: preview.items,
        });
    }

    fn apply(&mut self, preview_id: &str, include: Vec<String>) {
        if self.running.load(Ordering::SeqCst) {
            return self.refuse(IMPORT_RUNNING);
        }
        let Some(pending) = self.pending.take_if(|p| p.id == preview_id) else {
            return self.refuse(UNKNOWN_PREVIEW);
        };
        if pending.created.elapsed() > self.config.preview_ttl {
            return self.refuse(UNKNOWN_PREVIEW);
        }
        if lock(&self.rules).revision() != pending.revision {
            return self.refuse(STALE_PREVIEW);
        }
        let rules: Vec<Rule> = include
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|name| pending.rules.get(&name).cloned())
            .collect();
        info!(rules = rules.len(), "applying a rule import");
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let broadcast = self.broadcast.clone();
        let hold = self.commands.hold_rule_publishes();
        let applier = apply::Applier {
            commands: self.commands.clone(),
            broadcast: self.broadcast.clone(),
            reply_timeout: self.config.reply_timeout,
            retry_delay: self.config.retry_delay,
        };
        tokio::spawn(async move {
            let totals = apply::run(&applier, rules).await;
            // One `SetRules` with every confirmed rule, before the result.
            drop(hold);
            running.store(false, Ordering::SeqCst);
            let _ = broadcast.send(ServerMessage::RulesImportResult {
                applied: totals.applied,
                rejected: totals.rejected,
                not_sent: totals.not_sent,
                no_answer: totals.no_answer,
            });
        });
    }
}

#[cfg(test)]
mod tests;
