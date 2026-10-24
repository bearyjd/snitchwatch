use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const WAIT_TIMEOUT: Duration = Duration::from_secs(15);

/// One session-correlated request and its exact, immutable returned artifact.
/// Intentionally has no Debug implementation: artifacts are not diagnostic data.
#[derive(Default)]
pub struct ExportView {
    current: Option<Request>,
}
struct Request {
    session: u64,
    request_id: String,
    deadline: Instant,
    artifact: Option<Arc<str>>,
}
impl ExportView {
    /// Starting a request drops any previous preview or pending delivery.
    pub fn begin(&mut self, session: u64, request_id: String, now: Instant) {
        self.current = Some(Request {
            session,
            request_id,
            deadline: now + WAIT_TIMEOUT,
            artifact: None,
        });
    }

    /// Only the first timely response to the current request can supply bytes.
    pub fn accept(
        &mut self,
        session: u64,
        request_id: &str,
        artifact: Arc<str>,
        now: Instant,
    ) -> bool {
        let Some(current) = self.current.as_ref() else {
            return false;
        };
        if current.session != session
            || current.request_id != request_id
            || current.artifact.is_some()
        {
            return false;
        }
        if self.expire(now) {
            return false;
        }
        if let Some(current) = self.current.as_mut() {
            current.artifact = Some(artifact);
        }
        true
    }

    pub fn cancel(&mut self) {
        self.current = None;
    }

    /// Expires only an awaiting request; an accepted preview has no viewing timer.
    pub fn expire(&mut self, now: Instant) -> bool {
        let expired = self
            .current
            .as_ref()
            .is_some_and(|current| current.artifact.is_none() && now >= current.deadline);
        if expired {
            self.current = None;
        }
        expired
    }

    pub fn disconnect(&mut self) {
        self.cancel();
    }

    /// Freeze this Arc before admitting the controller's off-Qt Save job.
    pub fn artifact_for_save(&self, current_session: u64) -> Option<Arc<str>> {
        self.current
            .as_ref()
            .filter(|current| current.session == current_session)
            .and_then(|current| current.artifact.clone())
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
