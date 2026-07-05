//! Unexpected processes listening on user-reachable ports.
//!
//! A new listening socket — a bind shell, an unexpected service, a
//! debug/backdoor port — is a strong signal. We enumerate listening TCP/UDP
//! sockets via `ss -H -tulnp` and treat each as a trust-on-first-use item
//! keyed by `netid + local address`; a listener that appears (or whose
//! owning process changes) after the baseline is flagged.
//!
//! If `ss` is not installed the check reports [`CheckOutcome::Skipped`].

use super::{Check, CheckError, CheckOutcome, ScanContext, SurfaceItem};

pub struct ListenersCheck;

/// Parse `ss -H -tulnp` output into surface items. Pure — the parsing is
/// unit-tested against captured `ss` output with no `ss` present.
///
/// Columns (headerless): `netid state recv-q send-q local peer [process]`.
fn parse_ss(output: &str) -> Vec<SurfaceItem> {
    let mut items = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let netid = fields[0];
        let state = fields[1];
        // `-l` already restricts to listeners, but be defensive.
        if !matches!(state, "LISTEN" | "UNCONN") {
            continue;
        }
        let local = fields[4];
        // Process column is everything from field 6 onward (it can contain
        // spaces inside the users:(("name",pid=..)) form only rarely, but we
        // join to be safe). Absent when ss lacks permission to read it.
        let process = if fields.len() > 6 {
            fields[6..].join(" ")
        } else {
            "unknown-process".to_string()
        };
        let key = format!("listener:{netid}:{local}");
        items.push(SurfaceItem {
            detail: format!("listening socket {local} ({netid}) owned by {process}"),
            value: process,
            key,
        });
    }
    items
}

impl Check for ListenersCheck {
    fn name(&self) -> &'static str {
        "listeners"
    }

    fn surface(&self) -> &'static str {
        "listeners"
    }

    fn run(&self, ctx: &ScanContext<'_>) -> Result<CheckOutcome, CheckError> {
        if !ctx.sys.which("ss") {
            return Ok(CheckOutcome::Skipped {
                reason: "ss binary not found on PATH".to_string(),
            });
        }
        let out = ctx.sys.run("ss", &["-H", "-tulnp"])?;
        let current = parse_ss(&out.stdout);
        super::diff_surface(ctx, self.name(), self.surface(), current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::scans::ScanStore;
    use crate::testkit::MockInspector;
    use std::path::PathBuf;

    const SS_BASE: &str = "\
tcp   LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=800,fd=3))
udp   UNCONN 0 0   127.0.0.1:323 0.0.0.0:* users:((\"chronyd\",pid=750,fd=5))
";

    const SS_WITH_BINDSHELL: &str = "\
tcp   LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=800,fd=3))
udp   UNCONN 0 0   127.0.0.1:323 0.0.0.0:* users:((\"chronyd\",pid=750,fd=5))
tcp   LISTEN 0 1   0.0.0.0:4444 0.0.0.0:* users:((\"nc\",pid=31337,fd=3))
";

    fn ctx<'a>(sys: &'a MockInspector, scans: &'a ScanStore) -> ScanContext<'a> {
        ScanContext {
            sys,
            deployment: None,
            home: PathBuf::from("/home/tester"),
            scans,
        }
    }

    #[test]
    fn parse_extracts_listeners() {
        let items = parse_ss(SS_BASE);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].key, "listener:tcp:0.0.0.0:22");
        assert!(items[0].value.contains("sshd"));
    }

    #[test]
    fn parse_skips_malformed_and_non_listener_lines() {
        let out = "garbage\ntcp ESTAB 0 0 a b proc\n";
        assert!(parse_ss(out).is_empty());
    }

    #[test]
    fn skipped_when_ss_absent() {
        let sys = MockInspector::new();
        let scans = ScanStore::open_in_memory().unwrap();
        assert!(matches!(
            ListenersCheck.run(&ctx(&sys, &scans)).unwrap(),
            CheckOutcome::Skipped { .. }
        ));
    }

    #[test]
    fn new_bindshell_after_baseline_is_flagged() {
        let scans = ScanStore::open_in_memory().unwrap();
        let base = MockInspector::new().with_stdout("ss", &["-H", "-tulnp"], SS_BASE);
        assert!(matches!(
            ListenersCheck.run(&ctx(&base, &scans)).unwrap(),
            CheckOutcome::Ran {
                baselined: true,
                ..
            }
        ));

        let drifted = MockInspector::new().with_stdout("ss", &["-H", "-tulnp"], SS_WITH_BINDSHELL);
        match ListenersCheck.run(&ctx(&drifted, &scans)).unwrap() {
            CheckOutcome::Ran { anomalies, .. } => {
                assert_eq!(anomalies.len(), 1);
                assert_eq!(anomalies[0].path, "listener:tcp:0.0.0.0:4444");
                assert!(anomalies[0].detail.contains("nc"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn same_listeners_produce_no_finding() {
        let scans = ScanStore::open_in_memory().unwrap();
        let sys = MockInspector::new().with_stdout("ss", &["-H", "-tulnp"], SS_BASE);
        ListenersCheck.run(&ctx(&sys, &scans)).unwrap();
        match ListenersCheck.run(&ctx(&sys, &scans)).unwrap() {
            CheckOutcome::Ran { anomalies, .. } => assert!(anomalies.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }
}
