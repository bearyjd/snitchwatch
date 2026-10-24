//! Bounded serialization and one artifact delivery slot per authenticated connection.
use super::{ExportControl, ExportDocument, ExportError, GroupOmissionReason};
use serde::Serialize;
use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tokio::{
    sync::Notify,
    time::{Duration, Instant as SendInstant},
};

pub const MAX_ARTIFACT_BYTES: usize = 1_048_576;
pub const MAX_EXPORT_WIRE_BYTES: usize = 1_052_658;
const REPLY_TTL: Duration = Duration::from_secs(5);

pub struct PreparedExport {
    pub artifact: Arc<str>,
    pub wire: Arc<str>,
}

/// Writes each complete JSON encoding under its actual cap; capacity never exceeds it.
struct CappedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl CappedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
        }
    }
    fn finish(self) -> Result<String, ExportError> {
        String::from_utf8(self.bytes).map_err(|_| ExportError::Internal)
    }
}
impl Write for CappedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|&end| end <= self.limit)
            .ok_or_else(|| io::Error::other("export size limit"))?;
        debug_assert!(end <= self.bytes.capacity());
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Success<'a> {
    action: &'static str,
    request_id: &'a str,
    artifact: &'a str,
}

pub fn prepare_reply(
    request_id: &str,
    document: ExportDocument,
    control: &ExportControl,
) -> Result<PreparedExport, ExportError> {
    prepare_reply_with_probe(request_id, document, control, |doc| {
        let mut artifact = CappedWriter::new(MAX_ARTIFACT_BYTES);
        serde_json::to_writer(&mut artifact, doc)
            .map_err(|_| GroupOmissionReason::ArtifactLimit)?;
        let artifact = artifact
            .finish()
            .map_err(|_| GroupOmissionReason::ArtifactLimit)?;
        let mut wire = CappedWriter::new(MAX_EXPORT_WIRE_BYTES);
        serde_json::to_writer(
            &mut wire,
            &Success {
                action: "cacheSummaryExport",
                request_id,
                artifact: &artifact,
            },
        )
        .map_err(|_| GroupOmissionReason::WireLimit)?;
        let wire = wire.finish().map_err(|_| GroupOmissionReason::WireLimit)?;
        Ok((artifact, wire))
    })
}

pub(super) fn prepare_reply_with_probe(
    request_id: &str,
    mut document: ExportDocument,
    control: &ExportControl,
    mut probe: impl FnMut(&ExportDocument) -> Result<(String, String), GroupOmissionReason>,
) -> Result<PreparedExport, ExportError> {
    if !crate::ws_messages::valid_request_id(request_id)
        || document.group_count() > super::MAX_EMITTED_GROUPS
    {
        return Err(ExportError::InvalidRequest);
    }
    // At most 12 actual attempts for the core's at most 2,000 groups.
    loop {
        control.check()?;
        match probe(&document) {
            Ok((artifact, wire)) => {
                control.check()?;
                return Ok(PreparedExport {
                    artifact: artifact.into(),
                    wire: wire.into(),
                });
            }
            Err(reason) => {
                control.check()?;
                let count = document.group_count();
                if count == 0 {
                    return Err(ExportError::TooLarge);
                }
                document.trim_groups(count / 2, reason)?;
            }
        }
    }
}

#[derive(Clone)]
pub struct ExportSession {
    inner: Arc<Inner>,
}
struct Inner {
    state: Mutex<State>,
    changed: Notify,
}
struct State {
    closed: bool,
    generation: u64,
    active: Option<Active>,
    slot: Option<Slot>,
}
struct Active {
    generation: u64,
    request_id: String,
    control: ExportControl,
    published: bool,
}
struct Slot {
    generation: u64,
    request_id: String,
    wire: Option<Arc<str>>,
    expires_at: SendInstant,
    valid: bool,
}

impl Default for ExportSession {
    fn default() -> Self {
        Self::new()
    }
}
impl std::fmt::Debug for ExportSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExportSession")
    }
}
impl PartialEq for ExportSession {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}
/// Connection metadata that retains neither the session nor its pending artifact.
pub struct WeakExportSession(std::sync::Weak<Inner>);
impl WeakExportSession {
    pub fn upgrade(&self) -> Option<ExportSession> {
        self.0.upgrade().map(|inner| ExportSession { inner })
    }
}

impl ExportSession {
    /// Used by service shutdown without retaining every exporting connection.
    pub fn downgrade(&self) -> WeakExportSession {
        WeakExportSession(Arc::downgrade(&self.inner))
    }
    #[cfg(test)]
    pub(super) fn queued_wire(&self) -> Option<Arc<str>> {
        self.inner
            .state
            .lock()
            .unwrap()
            .slot
            .as_ref()
            .and_then(|slot| slot.wire.clone())
    }
    /// Either WebSocket-side lifetime guard permanently closes the session.
    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().unwrap().closed
    }
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    closed: false,
                    generation: 0,
                    active: None,
                    slot: None,
                }),
                changed: Notify::new(),
            }),
        }
    }
    pub fn admit(&self, request_id: &str, deadline: Instant) -> Result<ExportRun, ExportError> {
        if !crate::ws_messages::valid_request_id(request_id) {
            return Err(ExportError::InvalidRequest);
        }
        let mut state = self.inner.state.lock().unwrap();
        if state.closed {
            return Err(ExportError::Unavailable);
        }
        // An untouched expired slot can be discarded; a taken slot belongs to its sender guard.
        if state
            .slot
            .as_ref()
            .is_some_and(|slot| slot.wire.is_some() && SendInstant::now() >= slot.expires_at)
        {
            state.slot = None;
        }
        if state.active.is_some() || state.slot.is_some() {
            return Err(ExportError::Busy);
        }
        let control = ExportControl {
            deadline,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        control.check()?;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(ExportError::Unavailable)?;
        let generation = state.generation;
        state.active = Some(Active {
            generation,
            request_id: request_id.into(),
            control: control.clone(),
            published: false,
        });
        Ok(ExportRun {
            session: self.clone(),
            generation,
            control,
        })
    }
    pub fn cancel(&self, request_id: &str) {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(active) = &state.active {
            if active.request_id == request_id {
                active.control.cancelled.store(true, Ordering::Release);
            }
        }
        if let Some(slot) = &mut state.slot {
            if slot.request_id == request_id {
                slot.valid = false;
                if slot.wire.is_some() {
                    state.slot = None;
                }
            }
        }
        drop(state);
        self.inner.changed.notify_waiters();
    }
    pub fn close(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.closed = true;
        if let Some(active) = &state.active {
            active.control.cancelled.store(true, Ordering::Release);
        }
        if let Some(slot) = &mut state.slot {
            slot.valid = false;
            if slot.wire.is_some() {
                state.slot = None;
            }
        }
        drop(state);
        self.inner.changed.notify_waiters();
    }
    /// One connection-scoped future removes expired queued bytes even while
    /// its sink is blocked on unrelated traffic. Taken slots stay sender-owned.
    pub(crate) async fn expire_queued_replies(&self) {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let pending = {
                let state = self.inner.state.lock().unwrap();
                if state.closed {
                    return;
                }
                state
                    .slot
                    .as_ref()
                    .filter(|slot| slot.wire.is_some())
                    .map(|slot| (slot.generation, slot.expires_at))
            };
            if let Some((generation, expires_at)) = pending {
                tokio::select! {
                    _ = changed => {},
                    _ = tokio::time::sleep_until(expires_at) => {
                        let mut state = self.inner.state.lock().unwrap();
                        if state.slot.as_ref().is_some_and(|slot| slot.generation == generation && slot.wire.is_some()) {
                            state.slot = None;
                        }
                        drop(state);
                        self.inner.changed.notify_waiters();
                    }
                }
            } else {
                changed.await;
            }
        }
    }
    pub fn drop_guard(&self) -> ExportSessionGuard {
        ExportSessionGuard(self.clone())
    }
    pub async fn next_reply(&self) -> Option<PendingExportReply> {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.inner.state.lock().unwrap();
                if state.closed {
                    return None;
                }
                if state.slot.as_ref().is_some_and(|slot| {
                    slot.wire.is_some() && (!slot.valid || SendInstant::now() >= slot.expires_at)
                }) {
                    state.slot = None;
                }
                if let Some(slot) = &mut state.slot {
                    if let Some(wire) = slot.wire.take() {
                        return Some(PendingExportReply {
                            wire,
                            expires_at: slot.expires_at,
                            generation: slot.generation,
                            session: self.clone(),
                        });
                    }
                }
            }
            changed.await;
        }
    }
}
pub struct ExportSessionGuard(ExportSession);
impl Drop for ExportSessionGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub struct ExportRun {
    session: ExportSession,
    generation: u64,
    control: ExportControl,
}
impl ExportRun {
    pub fn control(&self) -> &ExportControl {
        &self.control
    }
    pub fn publish(&self, prepared: PreparedExport) -> bool {
        if self.control.check().is_err() {
            return false;
        }
        let mut state = self.session.inner.state.lock().unwrap();
        if state.closed
            || state.slot.is_some()
            || self.control.check().is_err()
            || !state
                .active
                .as_ref()
                .is_some_and(|active| active.generation == self.generation && !active.published)
        {
            return false;
        }
        let active = state.active.as_mut().unwrap();
        active.published = true;
        let request_id = active.request_id.clone();
        state.slot = Some(Slot {
            generation: self.generation,
            request_id,
            wire: Some(prepared.wire),
            expires_at: SendInstant::now() + REPLY_TTL,
            valid: true,
        });
        drop(state);
        self.session.inner.changed.notify_waiters();
        true
    }
}
impl Drop for ExportRun {
    fn drop(&mut self) {
        let mut state = self.session.inner.state.lock().unwrap();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == self.generation)
        {
            state.active = None;
        }
        drop(state);
        self.session.inner.changed.notify_waiters();
    }
}

pub struct PendingExportReply {
    pub wire: Arc<str>,
    pub expires_at: SendInstant,
    generation: u64,
    session: ExportSession,
}
impl PendingExportReply {
    pub fn is_current(&self) -> bool {
        let state = self.session.inner.state.lock().unwrap();
        !state.closed
            && state.slot.as_ref().is_some_and(|slot| {
                slot.generation == self.generation
                    && slot.valid
                    && SendInstant::now() < slot.expires_at
            })
    }
    /// Ends an in-flight send immediately when the request or connection is invalidated.
    pub async fn invalidated(&self) {
        loop {
            let changed = self.session.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.is_current() {
                return;
            }
            tokio::select! { _ = changed => {}, _ = tokio::time::sleep_until(self.expires_at) => return }
        }
    }
}
impl Drop for PendingExportReply {
    fn drop(&mut self) {
        let mut state = self.session.inner.state.lock().unwrap();
        if state
            .slot
            .as_ref()
            .is_some_and(|slot| slot.generation == self.generation)
        {
            state.slot = None;
        }
        drop(state);
        self.session.inner.changed.notify_waiters();
    }
}
