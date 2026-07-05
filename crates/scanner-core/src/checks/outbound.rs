//! Unusual outbound connections — **DEFERRED** for this release.
//!
//! This check is a deliberate no-op, not an oversight. Per
//! `IMPLEMENTATION_PROMPT.md` Phase 5's precondition, the outbound-connection
//! signal depends on Component A persisting its connection log and exposing a
//! stable read contract for Component B. That connection log is currently
//! in-memory only (`crates/snitchwatch-bridge/src/cache/`) and the
//! stop-and-ask on building that persistence resolved **"not needed yet"** —
//! Component B's userspace tier ships standalone for its first release.
//!
//! The check is wired into the registry as [`CheckOutcome::Deferred`] so the
//! scan report explicitly lists it as deferred rather than hiding the gap.
//! When Component A grows a persisted connection log + read contract, this is
//! where the "unusual outbound destination vs. the connection-log baseline"
//! logic lands, reusing the same [`crate::checks::diff_surface`] machinery.

use super::{Check, CheckError, CheckOutcome, ScanContext};

pub struct OutboundCheck;

/// Reason surfaced in the scan report's `deferred_checks` bucket.
pub const DEFER_REASON: &str =
    "outbound-connection analysis needs Component A's persisted connection-log \
     signal channel, which is not built yet (shipping Component B userspace tier standalone)";

impl Check for OutboundCheck {
    fn name(&self) -> &'static str {
        "outbound"
    }

    fn surface(&self) -> &'static str {
        "outbound"
    }

    fn run(&self, _ctx: &ScanContext<'_>) -> Result<CheckOutcome, CheckError> {
        Ok(CheckOutcome::Deferred {
            reason: DEFER_REASON.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::scans::ScanStore;
    use crate::testkit::MockInspector;
    use std::path::PathBuf;

    #[test]
    fn outbound_is_deferred() {
        let sys = MockInspector::new();
        let scans = ScanStore::open_in_memory().unwrap();
        let ctx = ScanContext {
            sys: &sys,
            deployment: None,
            home: PathBuf::from("/home/tester"),
            scans: &scans,
        };
        assert!(matches!(
            OutboundCheck.run(&ctx).unwrap(),
            CheckOutcome::Deferred { .. }
        ));
    }
}
