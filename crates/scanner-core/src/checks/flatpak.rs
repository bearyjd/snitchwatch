//! Flatpak permission (override) drift.
//!
//! Flatpak apps ship a static permission set, but users (or a malicious
//! script) can widen them with `flatpak override` — e.g. granting
//! `--filesystem=host` or `--share=network` to a sandboxed app. Those
//! user overrides are exactly the drift surface: we snapshot each app's (and
//! the global) override set and flag any that appears or changes after the
//! baseline.
//!
//! If `flatpak` is not installed the check reports [`CheckOutcome::Skipped`]
//! rather than silently passing.

use super::{diff_surface, sha256_hex, Check, CheckError, CheckOutcome, ScanContext, SurfaceItem};
use crate::inspector::SystemInspector;

pub struct FlatpakCheck;

/// Normalise override text so cosmetic ordering/whitespace differences don't
/// register as drift: trim, drop blank lines, sort.
fn normalise_overrides(raw: &str) -> String {
    let mut lines: Vec<&str> = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    lines.sort_unstable();
    lines.join("\n")
}

/// List installed application ids via `flatpak list`.
fn list_apps(sys: &dyn SystemInspector) -> Vec<String> {
    let Ok(out) = sys.run("flatpak", &["list", "--app", "--columns=application"]) else {
        return Vec::new();
    };
    out.stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

fn override_item(sys: &dyn SystemInspector, app: Option<&str>) -> Option<SurfaceItem> {
    let mut args = vec!["override", "--show", "--user"];
    if let Some(app) = app {
        args.push(app);
    }
    let out = sys.run("flatpak", &args).ok()?;
    let normalised = normalise_overrides(&out.stdout);
    if normalised.is_empty() {
        // No overrides for this scope -> nothing to track.
        return None;
    }
    let label = app.unwrap_or("__global__");
    Some(SurfaceItem {
        key: format!("flatpak-override:{label}"),
        value: sha256_hex(&normalised),
        detail: format!("Flatpak permission override changed for {label}: {normalised}"),
    })
}

impl Check for FlatpakCheck {
    fn name(&self) -> &'static str {
        "flatpak"
    }

    fn surface(&self) -> &'static str {
        "flatpak"
    }

    fn run(&self, ctx: &ScanContext<'_>) -> Result<CheckOutcome, CheckError> {
        if !ctx.sys.which("flatpak") {
            return Ok(CheckOutcome::Skipped {
                reason: "flatpak binary not found on PATH".to_string(),
            });
        }
        let mut current = Vec::new();
        if let Some(global) = override_item(ctx.sys, None) {
            current.push(global);
        }
        for app in list_apps(ctx.sys) {
            if let Some(item) = override_item(ctx.sys, Some(&app)) {
                current.push(item);
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

    fn ctx<'a>(sys: &'a MockInspector, scans: &'a ScanStore) -> ScanContext<'a> {
        ScanContext {
            sys,
            deployment: None,
            home: PathBuf::from("/home/tester"),
            scans,
        }
    }

    #[test]
    fn skipped_when_flatpak_absent() {
        let sys = MockInspector::new();
        let scans = ScanStore::open_in_memory().unwrap();
        assert!(matches!(
            FlatpakCheck.run(&ctx(&sys, &scans)).unwrap(),
            CheckOutcome::Skipped { .. }
        ));
    }

    #[test]
    fn new_dangerous_override_after_baseline_is_flagged() {
        let scans = ScanStore::open_in_memory().unwrap();
        // Baseline: firefox present, no overrides anywhere.
        let base = MockInspector::new()
            .with_stdout(
                "flatpak",
                &["list", "--app", "--columns=application"],
                "org.mozilla.firefox\n",
            )
            .with_stdout("flatpak", &["override", "--show", "--user"], "")
            .with_stdout(
                "flatpak",
                &["override", "--show", "--user", "org.mozilla.firefox"],
                "",
            );
        assert!(matches!(
            FlatpakCheck.run(&ctx(&base, &scans)).unwrap(),
            CheckOutcome::Ran {
                baselined: true,
                ..
            }
        ));

        // Now firefox gains filesystem=host.
        let drifted = MockInspector::new()
            .with_stdout(
                "flatpak",
                &["list", "--app", "--columns=application"],
                "org.mozilla.firefox\n",
            )
            .with_stdout("flatpak", &["override", "--show", "--user"], "")
            .with_stdout(
                "flatpak",
                &["override", "--show", "--user", "org.mozilla.firefox"],
                "[Context]\nfilesystems=host;\n",
            );
        match FlatpakCheck.run(&ctx(&drifted, &scans)).unwrap() {
            CheckOutcome::Ran { anomalies, .. } => {
                assert_eq!(anomalies.len(), 1);
                assert_eq!(anomalies[0].path, "flatpak-override:org.mozilla.firefox");
                assert!(anomalies[0].detail.contains("filesystems=host"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cosmetic_reordering_is_not_drift() {
        let scans = ScanStore::open_in_memory().unwrap();
        let mk = |body: &str| {
            MockInspector::new()
                .with_stdout(
                    "flatpak",
                    &["list", "--app", "--columns=application"],
                    "org.x.App\n",
                )
                .with_stdout("flatpak", &["override", "--show", "--user"], "")
                .with_stdout(
                    "flatpak",
                    &["override", "--show", "--user", "org.x.App"],
                    body,
                )
        };
        FlatpakCheck
            .run(&ctx(
                &mk("[Context]\nfeatures=devel;\nsockets=x11;\n"),
                &scans,
            ))
            .unwrap();
        // Same overrides, different order/whitespace.
        match FlatpakCheck
            .run(&ctx(
                &mk("  sockets=x11;\n[Context]\n\nfeatures=devel;"),
                &scans,
            ))
            .unwrap()
        {
            CheckOutcome::Ran { anomalies, .. } => assert!(anomalies.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }
}
