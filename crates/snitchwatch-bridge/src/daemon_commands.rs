//! Outbound daemon commands, reply correlation, and which daemon stream the
//! rules list comes from (issue #48).
//!
//! opensnitchd opens one bidirectional `Notifications` stream per connection
//! and answers every command with a `NotificationReply` carrying the
//! command's id: `OK`, or `ERROR` with the reason in `data`
//! (`vendor/opensnitch/daemon/ui/notifications.go` `sendNotificationReply`).
//! Its first message on a new stream is a HELLO, a reply with id 0
//! (`listenForNotifications`).
//!
//! **Current stream.** The open stream with the newest HELLO. When it closes,
//! "current" falls back to the newest still-open stream that sent a HELLO.
//! A non-zero reply counts only when it arrives on the stream that is
//! current at reply time (and, on the Unix transport, the stream the command
//! went to); other replies are logged and ignored. An `OK` for a rule
//! command updates the rules cache right here, in reply order.
//!
//! **Rules list.** A HELLO commits its connection's staged `Subscribe`
//! snapshot while this module's lock is held, so the committing stream is
//! current at that moment. The list belongs to that stream: when it closes,
//! or another stream becomes current without a snapshot of its own, the list
//! is withdrawn (cache `Unknown`, empty `SetRules`) rather than left standing
//! under a different stream.
//!
//! **Delivery, by transport.**
//! - [`DaemonTransport::Tcp`] (legacy per-user mode, `127.0.0.1`): every
//!   command fans out to every open stream, so a fake local "daemon" can
//!   never stop the real one from receiving it. `send` reports
//!   [`SendError::NoDaemon`] only when no stream is open at all, and
//!   in-flight waiters fail with [`CommandError::StreamClosed`] only when the
//!   last stream closes.
//!
//!   **Residual risk until #35 retires this transport.** Any local process
//!   can subscribe and send a later HELLO. While its stream is current, the
//!   Rules page shows *its* list and *its* replies decide command outcomes,
//!   and a toggle the user makes on one of its forged rows is sent, forged
//!   body and all, to every stream: the real daemon installs that body. The
//!   list is withdrawn the moment the stream closes or stops being current.
//! - [`DaemonTransport::Unix`] (system mode, root-only socket): commands go
//!   only to the current stream, and its waiters fail when that stream
//!   closes.
//!
//! Each stream has its own bounded queue fed with `try_send`, so a stream
//! that never reads drops its own commands instead of stalling the others.
//! The number of open streams is deliberately not capped: on TCP a cap would
//! let a local process fill it and lock the real daemon's stream out.

use crate::cache::rules::RulesSync;
use snitchwatch_proto::protocol::{Action, Notification, NotificationReply, NotificationReplyCode};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tracing::{info, warn};

/// Which daemon connection a `Subscribe` or a `Notifications` stream came
/// from: the TCP peer address in legacy mode, `None` on the Unix socket
/// (tonic's `UdsConnectInfo` has no remote address) and for in-process
/// `Request::new` calls. `None` is one shared key; every system-mode peer is
/// root (`RootUnixIncoming`).
pub type ConnKey = Option<SocketAddr>;

/// Identifies one open `Notifications` stream.
pub type StreamId = u64;

/// Per-stream outbound queue. Rule commands are user-paced (one click each),
/// so this only absorbs a burst such as a batch delete.
const STREAM_QUEUE_CAPACITY: usize = 64;

/// How long after its waiter timed out a command's `OK` is still applied to
/// the rules cache (with a warning) instead of being ignored.
const LATE_REPLY_GRACE: Duration = Duration::from_secs(30);

/// How the daemon reaches this bridge; see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DaemonTransport {
    #[default]
    Tcp,
    Unix,
}

/// The only notification types this bridge ever sends the root daemon. Any
/// other type is dropped at [`DaemonCommands::send`]: `CHANGE_CONFIG` in
/// particular can repoint the daemon's rules path, config paths, server
/// address and TLS files, and `NONE` closes the daemon's stream.
const ALLOWED_ACTIONS: [Action; 2] = [Action::ChangeRule, Action::DeleteRule];

/// Why [`DaemonCommands::send`] sent nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendError {
    /// Not a rule command; see [`ALLOWED_ACTIONS`].
    NotAllowed,
    /// A rule name fails [`crate::rule_name::validate_rule_name`].
    InvalidRuleName,
    /// No stream to send to: none open (TCP), or no current one (Unix).
    NoDaemon,
    /// Every target stream's queue was full or closing.
    NotQueued,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAllowed => "not a rule command; refused",
            Self::InvalidRuleName => {
                "a rule name could leave the daemon's rules directory; refused"
            }
            Self::NoDaemon => "no daemon connected",
            Self::NotQueued => "no daemon stream could queue the command",
        })
    }
}

impl std::error::Error for SendError {}

/// Why a command did not get an `OK` from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// The daemon replied `ERROR`; its reason text.
    Rejected(String),
    Timeout,
    StreamClosed,
}

/// Proof that a HELLO just made `stream` current, valid only while this
/// module's lock is held: it borrows the locked state, so it cannot outlive
/// the lock or be built anywhere else. [`RulesSync::commit`] requires one.
///
/// ```compile_fail
/// // Only `DaemonCommands::on_reply` can say a stream is current.
/// let _ = snitchwatch_bridge::daemon_commands::BecameCurrent {
///     stream: 1,
///     conn: None,
///     _locked: std::marker::PhantomData,
/// };
/// ```
pub struct BecameCurrent<'locked> {
    stream: StreamId,
    conn: ConnKey,
    _locked: PhantomData<&'locked Inner>,
}

impl<'locked> BecameCurrent<'locked> {
    fn new(_locked: &'locked Inner, stream: StreamId, conn: ConnKey) -> Self {
        Self {
            stream,
            conn,
            _locked: PhantomData,
        }
    }

    pub fn stream(&self) -> StreamId {
        self.stream
    }

    pub fn conn(&self) -> ConnKey {
        self.conn
    }
}

type Outcome = Result<(), CommandError>;

struct OpenStream {
    conn: ConnKey,
    hello: Option<u64>,
    tx: mpsc::Sender<Notification>,
}

struct Waiter {
    /// Unix mode: the stream the command went to. `None` in TCP mode, where
    /// the command went to every stream.
    stream: Option<StreamId>,
    command: Notification,
    tx: oneshot::Sender<Outcome>,
}

struct LateCommand {
    stream: Option<StreamId>,
    command: Notification,
    timed_out_at: Instant,
}

struct Inner {
    transport: DaemonTransport,
    next_stream: StreamId,
    next_hello: u64,
    /// Starts at 1: id 0 is the daemon's HELLO.
    next_id: u64,
    streams: HashMap<StreamId, OpenStream>,
    current: Option<StreamId>,
    /// The stream whose snapshot the rules cache holds. Always `None` or
    /// `current`.
    committed_by: Option<StreamId>,
    waiters: HashMap<u64, Waiter>,
    late: HashMap<u64, LateCommand>,
}

/// Shared handle; clones see the same streams and waiters.
#[derive(Clone)]
pub struct DaemonCommands {
    inner: Arc<StdMutex<Inner>>,
    ready: Arc<watch::Sender<u64>>,
    rules: RulesSync,
}

/// Keeps a stream registered; dropping it closes the stream (also on a
/// panic in the reply loop, so no ghost stream keeps TCP waiters pending).
pub struct StreamRegistration {
    id: StreamId,
    commands: DaemonCommands,
}

/// A sent command's reply. Dropping it forgets the waiter.
pub struct PendingReply {
    id: u64,
    rx: oneshot::Receiver<Outcome>,
    inner: Arc<StdMutex<Inner>>,
}

fn lock(inner: &StdMutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(|e| e.into_inner())
}

impl DaemonCommands {
    pub fn new(transport: DaemonTransport, rules: RulesSync) -> Self {
        Self {
            inner: Arc::new(StdMutex::new(Inner {
                transport,
                next_stream: 1,
                next_hello: 1,
                next_id: 1,
                streams: HashMap::new(),
                current: None,
                committed_by: None,
                waiters: HashMap::new(),
                late: HashMap::new(),
            })),
            ready: Arc::new(watch::channel(0).0),
            rules,
        }
    }

    /// Generation bumped by every HELLO, after its snapshot (if any) is
    /// committed. Wait with a level check (`wait_for(|g| *g >= n)`), not
    /// `changed()`, which misses a HELLO handled before the receiver was
    /// taken.
    pub fn stream_ready(&self) -> watch::Receiver<u64> {
        self.ready.subscribe()
    }

    /// Register a newly opened `Notifications` stream. The receiver yields
    /// the commands addressed to it and ends when the stream is closed.
    pub fn open_stream(&self, conn: ConnKey) -> (StreamRegistration, mpsc::Receiver<Notification>) {
        let (tx, rx) = mpsc::channel(STREAM_QUEUE_CAPACITY);
        let mut inner = lock(&self.inner);
        let id = inner.next_stream;
        inner.next_stream += 1;
        inner.streams.insert(
            id,
            OpenStream {
                conn,
                hello: None,
                tx,
            },
        );
        let registration = StreamRegistration {
            id,
            commands: self.clone(),
        };
        (registration, rx)
    }

    /// Handle one inbound reply on `stream`. Returns whether it was a HELLO
    /// that made `stream` current.
    pub fn on_reply(&self, stream: StreamId, reply: &NotificationReply) -> bool {
        let mut inner = lock(&self.inner);
        if reply.id == 0 {
            return self.hello(&mut inner, stream);
        }
        if inner.current != Some(stream) {
            // A stale stream after a redial, or another local process.
            warn!(
                stream,
                id = reply.id,
                "ignoring a reply from a non-current daemon stream"
            );
            return false;
        }
        let ok = reply.code == NotificationReplyCode::Ok as i32;
        if let Some(waiter) = inner.waiters.get(&reply.id) {
            if waiter.stream.is_some_and(|sent_to| sent_to != stream) {
                warn!(
                    stream,
                    id = reply.id,
                    "ignoring a reply from a stream the command was not sent to"
                );
                return false;
            }
            let waiter = inner.waiters.remove(&reply.id).expect("checked above");
            if ok {
                self.rules.apply_confirmed(&waiter.command);
            }
            let outcome = if ok {
                Ok(())
            } else {
                Err(CommandError::Rejected(reply.data.clone()))
            };
            let _ = waiter.tx.send(outcome);
        } else if let Some(late) = inner.late.remove(&reply.id) {
            let in_time = late.timed_out_at.elapsed() <= LATE_REPLY_GRACE;
            let right_stream = late.stream.is_none_or(|sent_to| sent_to == stream);
            if ok && in_time && right_stream {
                warn!(
                    id = reply.id,
                    "applying a daemon OK that arrived after its timeout"
                );
                self.rules.apply_confirmed(&late.command);
            } else {
                warn!(id = reply.id, ok, "ignoring a late daemon reply");
            }
        } else {
            warn!(id = reply.id, "ignoring a reply for an unknown command");
        }
        false
    }

    fn hello(&self, inner: &mut Inner, stream: StreamId) -> bool {
        let sequence = inner.next_hello;
        let Some(open) = inner.streams.get_mut(&stream) else {
            return false;
        };
        open.hello = Some(sequence);
        let conn = open.conn;
        inner.next_hello += 1;
        inner.current = Some(stream);
        let committed = self.rules.commit(&BecameCurrent::new(inner, stream, conn));
        if committed {
            inner.committed_by = Some(stream);
        } else if inner.committed_by.is_some_and(|by| by != stream) {
            self.rules.withdraw();
            inner.committed_by = None;
        }
        self.ready.send_modify(|generation| *generation += 1);
        info!(stream, committed, "daemon stream said HELLO; now current");
        true
    }

    /// Send a command, replacing its id with the next one. See the module
    /// doc for which streams receive it.
    pub fn send(&self, mut notification: Notification) -> Result<PendingReply, SendError> {
        if !ALLOWED_ACTIONS
            .iter()
            .any(|action| *action as i32 == notification.r#type)
        {
            warn!(
                action = notification.r#type,
                "refusing to send a non-rule command to the daemon"
            );
            return Err(SendError::NotAllowed);
        }
        // Defense in depth for every path that builds a command: the root
        // daemon turns the name into a file path.
        if let Some(error) = notification
            .rules
            .iter()
            .find_map(|rule| crate::rule_name::validate_rule_name(&rule.name).err())
        {
            warn!(%error, "refusing to send a rule command with an unsafe rule name");
            return Err(SendError::InvalidRuleName);
        }
        let mut inner = lock(&self.inner);
        let targets: Vec<StreamId> = match inner.transport {
            DaemonTransport::Tcp => inner.streams.keys().copied().collect(),
            DaemonTransport::Unix => inner.current.into_iter().collect(),
        };
        if targets.is_empty() {
            return Err(SendError::NoDaemon);
        }
        let id = inner.next_id;
        inner.next_id += 1;
        notification.id = id;
        let mut queued = 0;
        for target in targets {
            match inner.streams[&target].tx.try_send(notification.clone()) {
                Ok(()) => queued += 1,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!(
                        stream = target,
                        id, "daemon stream queue full; command dropped for it"
                    )
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {}
            }
        }
        if queued == 0 {
            return Err(SendError::NotQueued);
        }
        let (tx, rx) = oneshot::channel();
        let stream = match inner.transport {
            DaemonTransport::Tcp => None,
            DaemonTransport::Unix => inner.current,
        };
        inner.waiters.insert(
            id,
            Waiter {
                stream,
                command: notification,
                tx,
            },
        );
        Ok(PendingReply {
            id,
            rx,
            inner: self.inner.clone(),
        })
    }

    fn close_stream(&self, stream: StreamId) {
        let mut inner = lock(&self.inner);
        if inner.streams.remove(&stream).is_none() {
            return;
        }
        if inner.current == Some(stream) {
            // Fall back to the newest still-open stream that said HELLO.
            inner.current = inner
                .streams
                .iter()
                .filter_map(|(id, open)| open.hello.map(|hello| (hello, *id)))
                .max()
                .map(|(_, id)| id);
        }
        if inner.committed_by == Some(stream) {
            self.rules.withdraw();
            inner.committed_by = None;
        }
        let failed: Vec<u64> = match inner.transport {
            DaemonTransport::Tcp if inner.streams.is_empty() => {
                inner.waiters.keys().copied().collect()
            }
            DaemonTransport::Tcp => Vec::new(),
            DaemonTransport::Unix => inner
                .waiters
                .iter()
                .filter(|(_, waiter)| waiter.stream == Some(stream))
                .map(|(id, _)| *id)
                .collect(),
        };
        for id in failed {
            if let Some(waiter) = inner.waiters.remove(&id) {
                let _ = waiter.tx.send(Err(CommandError::StreamClosed));
            }
        }
        info!(stream, current = ?inner.current, "daemon stream closed");
    }
}

impl StreamRegistration {
    pub fn id(&self) -> StreamId {
        self.id
    }
}

impl Drop for StreamRegistration {
    fn drop(&mut self) {
        self.commands.close_stream(self.id);
    }
}

impl PendingReply {
    /// The id the daemon will echo back.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// On `Timeout`, the command is kept for [`LATE_REPLY_GRACE`] so a late
    /// `OK` still reaches the rules cache.
    pub async fn wait(mut self, timeout: Duration) -> Result<(), CommandError> {
        match tokio::time::timeout(timeout, &mut self.rx).await {
            Ok(Ok(outcome)) => outcome,
            // The waiter was dropped without an answer.
            Ok(Err(_)) => Err(CommandError::StreamClosed),
            Err(_) => {
                let mut inner = lock(&self.inner);
                if let Some(waiter) = inner.waiters.remove(&self.id) {
                    let now = Instant::now();
                    inner
                        .late
                        .retain(|_, late| now - late.timed_out_at <= LATE_REPLY_GRACE);
                    inner.late.insert(
                        self.id,
                        LateCommand {
                            stream: waiter.stream,
                            command: waiter.command,
                            timed_out_at: now,
                        },
                    );
                }
                Err(CommandError::Timeout)
            }
        }
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        lock(&self.inner).waiters.remove(&self.id);
    }
}

#[cfg(test)]
#[path = "daemon_commands/tests.rs"]
mod tests;
