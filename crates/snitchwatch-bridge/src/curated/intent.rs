//! Ephemeral intent state owned by one authenticated WebSocket connection.
//! Lock manager state before admission; keep this guard through the commit.
use crate::ws_messages::{valid_request_id, CuratedIntentPurpose};
use rand::RngCore;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const CONSENT_VERSION: &str = "resolver-any-address-v1";
pub const INTENT_LIFETIME: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentError {
    InvalidIntent,
    StaleIntent,
    Unavailable,
}

struct PendingIntent {
    request_id: String,
    id: String,
    purpose: CuratedIntentPurpose,
    fingerprint: String,
    revision: u64,
    expires: Instant,
    ticket: String,
}

struct ConnectionState {
    alive: bool,
    pending: Option<PendingIntent>,
}

/// Server-created authority context. It cannot be constructed from JSON.
#[derive(Clone)]
pub struct IntentContext(Arc<Mutex<ConnectionState>>);

impl std::fmt::Debug for IntentContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IntentContext")
    }
}

impl PartialEq for IntentContext {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl IntentContext {
    pub(crate) fn websocket() -> Self {
        Self(Arc::new(Mutex::new(ConnectionState {
            alive: true,
            pending: None,
        })))
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::websocket()
    }

    pub fn admit(&self) -> Result<IntentAdmission<'_>, IntentError> {
        let state = self.0.lock().map_err(|_| IntentError::Unavailable)?;
        if !state.alive {
            return Err(IntentError::StaleIntent);
        }
        Ok(IntentAdmission(state))
    }

    /// Both pump directions own a guard; cancellation/drop also kills the lease.
    pub(crate) fn drop_guard(&self) -> IntentDropGuard {
        IntentDropGuard(self.clone())
    }

    fn kill(&self) {
        // Poisoned state still must lose authority on termination.
        let mut state = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        state.alive = false;
        state.pending = None;
    }
}

pub(crate) struct IntentDropGuard(IntentContext);
impl Drop for IntentDropGuard {
    fn drop(&mut self) {
        self.0.kill();
    }
}

/// Admission and lease death serialize on this same guard.
pub struct IntentAdmission<'a>(MutexGuard<'a, ConnectionState>);

impl IntentAdmission<'_> {
    pub fn prepare(
        &mut self,
        request_id: &str,
        id: &str,
        purpose: CuratedIntentPurpose,
        fingerprint: &str,
        revision: u64,
    ) -> Result<String, IntentError> {
        self.invalidate();
        if !valid_request_id(request_id) || id != "dns-resolved" || !valid_ticket(fingerprint) {
            return Err(IntentError::InvalidIntent);
        }
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| IntentError::Unavailable)?;
        let ticket: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.0.pending = Some(PendingIntent {
            request_id: request_id.to_owned(),
            id: id.to_owned(),
            purpose,
            fingerprint: fingerprint.to_owned(),
            revision,
            expires: Instant::now() + INTENT_LIFETIME,
            ticket: ticket.clone(),
        });
        Ok(ticket)
    }

    /// Every submission consumes this connection's pending ticket, including
    /// mismatches. Caller independently validates its modern requestId/action.
    #[allow(clippy::too_many_arguments)]
    pub fn consume(
        &mut self,
        request_id: &str,
        ticket: &str,
        id: &str,
        purpose: CuratedIntentPurpose,
        fingerprint: &str,
        revision: u64,
        consent: &str,
    ) -> Result<(), IntentError> {
        let pending = self.0.pending.take().ok_or(IntentError::StaleIntent)?;
        if !valid_request_id(request_id)
            || !valid_ticket(ticket)
            || id != "dns-resolved"
            || consent != CONSENT_VERSION
        {
            return Err(IntentError::InvalidIntent);
        }
        if Instant::now() >= pending.expires
            || pending.request_id != request_id
            || pending.id != id
            || pending.purpose != purpose
            || pending.fingerprint != fingerprint
            || pending.revision != revision
            || !constant_time_equal(&pending.ticket, ticket)
        {
            return Err(IntentError::StaleIntent);
        }
        Ok(())
    }

    pub fn cancel(&mut self, request_id: &str, id: &str) -> bool {
        if self
            .0
            .pending
            .as_ref()
            .is_some_and(|p| p.request_id == request_id && p.id == id)
        {
            self.invalidate();
            true
        } else {
            false
        }
    }

    pub fn invalidate(&mut self) {
        self.0.pending = None;
    }
}

pub fn valid_ticket(ticket: &str) -> bool {
    ticket.len() == 64
        && ticket
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn constant_time_equal(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |difference, (x, y)| difference | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests;
