//! Unexpected systemd `--user` units and XDG autostart entries.
//!
//! Both are user-level persistence surfaces: a `~/.config/systemd/user`
//! unit (once enabled) or a `~/.config/autostart/*.desktop` entry runs code
//! at login without any privilege. They live under `$HOME` (rule 1 for the
//! `/usr` tree) so they get a trust-on-first-use baseline: a unit/entry that
//! appears — or whose contents change — after the baseline is flagged.

use super::{diff_surface, sha256_hex, Check, CheckError, CheckOutcome, ScanContext, SurfaceItem};

/// Unit-file suffixes we treat as systemd `--user` units.
const UNIT_SUFFIXES: &[&str] = &[
    ".service",
    ".socket",
    ".timer",
    ".target",
    ".path",
    ".slice",
    ".mount",
    ".automount",
    ".scope",
];

pub struct SystemdUserCheck;

impl SystemdUserCheck {
    fn collect_dir(
        &self,
        ctx: &ScanContext<'_>,
        rel_dir: &str,
        accept: impl Fn(&str) -> bool,
        kind: &str,
        into: &mut Vec<SurfaceItem>,
    ) -> Result<(), CheckError> {
        let dir = ctx.home.join(rel_dir);
        for path in ctx.sys.list_dir(&dir)? {
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if !accept(name) {
                continue;
            }
            let contents = ctx.sys.read_to_string(&path).unwrap_or_default();
            let key = path.to_string_lossy().into_owned();
            into.push(SurfaceItem {
                detail: format!("unexpected {kind}: {key}"),
                value: sha256_hex(&contents),
                key,
            });
        }
        Ok(())
    }
}

impl Check for SystemdUserCheck {
    fn name(&self) -> &'static str {
        "systemd-user"
    }

    fn surface(&self) -> &'static str {
        "systemd-user"
    }

    fn run(&self, ctx: &ScanContext<'_>) -> Result<CheckOutcome, CheckError> {
        let mut current = Vec::new();
        self.collect_dir(
            ctx,
            ".config/systemd/user",
            |name| UNIT_SUFFIXES.iter().any(|s| name.ends_with(s)),
            "systemd --user unit",
            &mut current,
        )?;
        self.collect_dir(
            ctx,
            ".config/autostart",
            |name| name.ends_with(".desktop"),
            "autostart entry",
            &mut current,
        )?;
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
        SystemdUserCheck.run(&ctx).unwrap()
    }

    #[test]
    fn new_autostart_entry_after_baseline_is_flagged() {
        let scans = ScanStore::open_in_memory().unwrap();
        // First run: empty autostart + empty units.
        let sys = MockInspector::new();
        assert!(matches!(
            run(&sys, &scans),
            CheckOutcome::Ran {
                baselined: true,
                ..
            }
        ));

        // A .desktop appears.
        let sys2 = MockInspector::new()
            .with_dir(
                "/home/tester/.config/autostart",
                vec![PathBuf::from("/home/tester/.config/autostart/evil.desktop")],
            )
            .with_file(
                "/home/tester/.config/autostart/evil.desktop",
                "[Desktop Entry]\nExec=/tmp/x\n",
            );
        match run(&sys2, &scans) {
            CheckOutcome::Ran { anomalies, .. } => {
                assert_eq!(anomalies.len(), 1);
                assert!(anomalies[0].path.ends_with("evil.desktop"));
                assert!(anomalies[0].detail.contains("autostart entry"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn new_user_service_after_baseline_is_flagged() {
        let scans = ScanStore::open_in_memory().unwrap();
        run(&MockInspector::new(), &scans); // baseline empty

        let unit = "/home/tester/.config/systemd/user/backdoor.service";
        let sys = MockInspector::new()
            .with_dir(
                "/home/tester/.config/systemd/user",
                vec![PathBuf::from(unit)],
            )
            .with_file(unit, "[Service]\nExecStart=/tmp/x\n");
        match run(&sys, &scans) {
            CheckOutcome::Ran { anomalies, .. } => {
                assert_eq!(anomalies.len(), 1);
                assert!(anomalies[0].path.ends_with("backdoor.service"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn non_unit_files_are_ignored() {
        let scans = ScanStore::open_in_memory().unwrap();
        run(&MockInspector::new(), &scans);
        // A README in the units dir must not be treated as a unit.
        let readme = "/home/tester/.config/systemd/user/README.md";
        let sys = MockInspector::new()
            .with_dir(
                "/home/tester/.config/systemd/user",
                vec![PathBuf::from(readme)],
            )
            .with_file(readme, "notes\n");
        match run(&sys, &scans) {
            CheckOutcome::Ran { anomalies, .. } => assert!(anomalies.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }
}
