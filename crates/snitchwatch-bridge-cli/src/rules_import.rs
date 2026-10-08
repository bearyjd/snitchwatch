//! Rule import and export for the bridge's GUIs (roadmap P2.7).
//!
//! One task owns the state: at most one pending preview (a new preview
//! replaces it; it expires after [`ImportConfig::preview_ttl`]) and whether
//! an apply is running. The inbound pump only routes the three messages
//! here ([`RulesImport::try_route`]) and never waits on them; parsing and
//! the rule policy (which compiles regexps) run on the blocking pool, off
//! the cache lock.
//!
//! Answers go only to the connection that asked, echoing its request id
//! (an apply's progress and result carry the preview id). Any
//! `snitchwatch-ui` member can already change rules, so a preview has no
//! per-session owner. Nothing is exported, previewed or applied on the
//! legacy TCP transport, where any local process can pose as the daemon
//! (#35).
//!
//! An apply is refused when its preview is unknown or expired, when the
//! rules cache's revision moved since the preview (any toggle, delete,
//! prompt rule, expiry or reconnect), while another apply runs, or for more
//! than [`MAX_APPLY_RULES`] rules. Import only ever sends `CHANGE_RULE`
//! ([`apply`]); it never deletes a rule.

use snitchwatch_bridge::auth::Token;
use snitchwatch_bridge::cache::rules::{RulesCache, SharedRulesCache};
use snitchwatch_bridge::daemon_commands::{DaemonCommands, DaemonTransport};
use snitchwatch_bridge::rule_io::{self, ExportUnavailable};
use snitchwatch_bridge::ws_messages::{ClientMessage, ReplyTo, ServerMessage};
use snitchwatch_proto::protocol::Rule;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

mod apply;
mod run_guard;

use crate::replier::Replier;
pub(crate) use run_guard::ApplyRun;

pub(crate) const UNKNOWN_PREVIEW: &str =
    "This import preview is no longer available. Preview the file again.";
pub(crate) const STALE_PREVIEW: &str = "Rules changed since the preview. Preview the file again.";
pub(crate) const IMPORT_RUNNING: &str =
    "Another import is still being applied. Wait for it to finish.";
pub(crate) const TCP_REFUSED: &str = "Rule import and export need the system Snitchwatch \
     service. In this per-user setup another program could pose as the firewall service.";
pub(crate) const TOO_MANY_AT_ONCE: &str = "Apply at most 2,000 changes at once. Untick some, \
     apply, then import the file again for the rest.";
const QUEUE_FULL: &str = "Snitchwatch is still working on earlier import requests. Try again.";
const INTERNAL: &str = "The rules couldn't be read because of an internal error.";

/// Most rules one apply sends.
pub(crate) const MAX_APPLY_RULES: usize = 2_000;
/// Import or export requests waiting for the task.
const REQUEST_QUEUE: usize = 8;

pub(crate) struct ImportConfig {
    /// How long each rule's daemon answer is awaited (#48's pump uses 5 s).
    pub(crate) reply_timeout: Duration,
    /// Pause before re-sending a command no stream could queue.
    pub(crate) retry_delay: Duration,
    /// How long a preview may be applied.
    pub(crate) preview_ttl: Duration,
    /// Names rule commands are changing (shared with them).
    pub(crate) busy: crate::busy::BusyNames,
}

impl Default for ImportConfig {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(5),
            retry_delay: Duration::from_millis(100),
            preview_ttl: Duration::from_secs(10 * 60),
            busy: crate::busy::BusyNames::default(),
        }
    }
}

/// The pump's handle to the import task.
pub(crate) struct RulesImport {
    tx: mpsc::Sender<ClientMessage>,
    broadcast: broadcast::Sender<ServerMessage>,
}

impl RulesImport {
    /// `busy`: the names rule commands are changing, shared with them.
    pub(crate) fn spawn(
        commands: DaemonCommands,
        rules: SharedRulesCache,
        broadcast: broadcast::Sender<ServerMessage>,
        busy: crate::busy::BusyNames,
    ) -> Self {
        let config = ImportConfig {
            busy,
            ..ImportConfig::default()
        };
        Self::spawn_with(commands, rules, broadcast, config)
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
        let (request_id, reply, is_export) = match &msg {
            ClientMessage::ExportRules { request_id, reply } => (request_id, reply, true),
            ClientMessage::PreviewRulesImport {
                request_id, reply, ..
            }
            | ClientMessage::ApplyRulesImport {
                request_id, reply, ..
            } => (request_id, reply, false),
            _ => return Some(msg),
        };
        let (request_id, replier) = (
            usable(request_id.clone()),
            Replier::new(reply.clone(), self.broadcast.clone()),
        );
        if self.tx.try_send(msg).is_err() {
            warn!("rule import queue full; request dropped");
            let reason = QUEUE_FULL.to_string();
            let answer = if is_export {
                ServerMessage::RulesExportUnavailable { request_id, reason }
            } else {
                ServerMessage::RulesImportRefused { request_id, reason }
            };
            // Never a task per dropped request: answer only if there is room.
            replier.send_now(answer);
        }
        None
    }
}

struct PendingPreview {
    id: String,
    created: Instant,
    revision: u64,
    /// The previewed `Add`/`Replace` rules, and the daemon rule each was
    /// compared with (`None`: an add), by name.
    rules: BTreeMap<String, (Rule, Option<Rule>)>,
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

/// A client's request id if it is usable (`valid_request_id`), else empty:
/// an answer never echoes anything else.
fn usable(request_id: String) -> String {
    if snitchwatch_bridge::ws_messages::valid_request_id(&request_id) {
        request_id
    } else {
        String::new()
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl ImportTask {
    async fn run(mut self, mut rx: mpsc::Receiver<ClientMessage>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                ClientMessage::ExportRules { request_id, reply } => {
                    self.export(self.replier(reply), usable(request_id)).await
                }
                ClientMessage::PreviewRulesImport {
                    request_id,
                    document,
                    reply,
                } => {
                    let request_id = usable(request_id);
                    self.preview(self.replier(reply), request_id, document)
                        .await
                }
                ClientMessage::ApplyRulesImport {
                    request_id,
                    preview_id,
                    include,
                    reply,
                } => {
                    let replier = self.replier(reply);
                    if let Err(reason) = self.apply(replier.clone(), &preview_id, include) {
                        let answer = ServerMessage::RulesImportRefused {
                            request_id: usable(request_id),
                            reason: reason.to_string(),
                        };
                        replier.send(answer).await;
                    }
                }
                _ => warn!("rule import task got a message it doesn't handle"),
            }
        }
    }

    fn replier(&self, reply: Option<ReplyTo>) -> Replier {
        Replier::new(reply, self.broadcast.clone())
    }

    fn on_tcp(&self) -> bool {
        self.commands.transport() == DaemonTransport::Tcp
    }

    async fn export(&self, replier: Replier, request_id: String) {
        let unavailable = |reason: &str| ServerMessage::RulesExportUnavailable {
            request_id: request_id.clone(),
            reason: reason.to_string(),
        };
        if self.on_tcp() {
            return replier.send(unavailable(TCP_REFUSED)).await;
        }
        // A copy, so the policy runs without holding the cache lock.
        let snapshot = lock(&self.rules).clone();
        let now_ms = now_unix_ms();
        let exported =
            tokio::task::spawn_blocking(move || rule_io::export(&snapshot, now_ms)).await;
        let answer = match exported {
            Ok(Ok(export)) => {
                info!(rules = export.document.rules.len(), "exported rules");
                ServerMessage::RulesExport {
                    request_id: request_id.clone(),
                    document: export.document,
                    omitted: export.omitted,
                }
            }
            Ok(Err(ExportUnavailable)) => unavailable(ExportUnavailable::REASON),
            Err(join_error) => {
                error!(error = %join_error, "rule export failed");
                unavailable(INTERNAL)
            }
        };
        replier.send(answer).await;
    }

    async fn preview(&mut self, replier: Replier, request_id: String, document: serde_json::Value) {
        self.pending = None;
        let answer = match self.check_preview(document).await {
            Ok((id, items)) => ServerMessage::RulesImportPreview {
                request_id,
                preview_id: id,
                items,
            },
            Err(reason) => ServerMessage::RulesImportRefused {
                request_id,
                reason: reason.to_string(),
            },
        };
        replier.send(answer).await;
    }

    /// Check a document and remember its preview; the preview id and items,
    /// or why it was refused.
    async fn check_preview(
        &mut self,
        document: serde_json::Value,
    ) -> Result<(String, Vec<rule_io::ImportItem>), &'static str> {
        if self.on_tcp() {
            return Err(TCP_REFUSED);
        }
        let checked = tokio::task::spawn_blocking(move || {
            rule_io::parse_document(document).map(|doc| rule_io::check_rules(&doc))
        })
        .await;
        let checked = match checked {
            Ok(Ok(checked)) => checked,
            Ok(Err(document_error)) => return Err(document_error.describe()),
            Err(join_error) => {
                error!(error = %join_error, "rule import preview failed");
                return Err(INTERNAL);
            }
        };
        let cache = lock(&self.rules);
        let preview = rule_io::classify(&checked, &cache).map_err(|e| e.describe())?;
        let cached = cache.rules();
        let rules = preview
            .applicable
            .into_iter()
            .map(|(name, rule)| {
                let before = cached.and_then(|rules| rules.get(&name)).cloned();
                (name, (rule, before))
            })
            .collect();
        let id = Token::generate().as_str()[..16].to_string();
        info!(rules = preview.items.len(), "previewed a rule import");
        self.pending = Some(PendingPreview {
            id: id.clone(),
            created: Instant::now(),
            revision: cache.revision(),
            rules,
        });
        Ok((id, preview.items))
    }

    /// Start applying the named rules of the pending preview, or say why not.
    fn apply(
        &mut self,
        replier: Replier,
        preview_id: &str,
        include: Vec<String>,
    ) -> Result<(), &'static str> {
        if self.on_tcp() {
            return Err(TCP_REFUSED);
        }
        if self.running.load(Ordering::SeqCst) {
            return Err(IMPORT_RUNNING);
        }
        let pending = self
            .pending
            .take_if(|p| p.id == preview_id)
            .ok_or(UNKNOWN_PREVIEW)?;
        if pending.created.elapsed() > self.config.preview_ttl {
            return Err(UNKNOWN_PREVIEW);
        }
        if lock(&self.rules).revision() != pending.revision {
            return Err(STALE_PREVIEW);
        }
        let names: BTreeSet<String> = include
            .into_iter()
            .filter(|name| pending.rules.contains_key(name))
            .collect();
        if names.len() > MAX_APPLY_RULES {
            self.pending = Some(pending);
            return Err(TOO_MANY_AT_ONCE);
        }
        let rules: Vec<(Rule, Option<Rule>)> = names
            .iter()
            .filter_map(|name| pending.rules.get(name).cloned())
            .collect();
        self.start_apply(replier, pending.id, rules);
        Ok(())
    }

    fn start_apply(&self, replier: Replier, preview_id: String, rules: Vec<(Rule, Option<Rule>)>) {
        info!(rules = rules.len(), "applying a rule import");
        self.running.store(true, Ordering::SeqCst);
        let run = ApplyRun::new(
            self.running.clone(),
            self.commands.hold_rule_publishes(),
            replier.clone(),
            preview_id.clone(),
        );
        let applier = apply::Applier::new(
            self.commands.clone(),
            self.rules.clone(),
            replier,
            preview_id,
            self.config.reply_timeout,
            self.config.retry_delay,
            self.config.busy.clone(),
        );
        tokio::spawn(async move {
            // The whole guard moves in (not just its `Copy` totals field).
            let mut run = run;
            apply::run(&applier, rules, &mut run.totals).await;
            // On a panic or cancellation above, dropping `run` does this.
            run.finish().await;
        });
    }
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod task_tests;

#[cfg(test)]
mod e2e_tests;
