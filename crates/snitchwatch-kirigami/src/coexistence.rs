//! Detect coexistence conflicts with upstream `opensnitch-ui`.
//!
//! Snitchwatch replaces the upstream OpenSnitch GUI; running both against
//! the same daemon contends for the UI gRPC channel and risks `ui.proto`
//! version skew (see README.md's "Coexistence with upstream opensnitch-ui"
//! section and `docs/packaging/rpm-ostree-layering.md`'s detect-and-disable
//! walkthrough). This module turns that manual `rpm -q`/`ls` doc check into
//! an actual runtime check surfaced in the Diagnostics page, so a user
//! doesn't have to remember to look for it themselves.

use std::path::Path;

/// Result of checking for an upstream `opensnitch-ui` install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CoexistenceReport {
    pub package_installed: bool,
    pub autostart_present: bool,
}

impl CoexistenceReport {
    pub fn conflict(&self) -> bool {
        self.package_installed || self.autostart_present
    }

    /// Human-readable summary for the Diagnostics page. Mirrors the
    /// detect-and-disable guidance already documented in README.md.
    pub fn message(&self) -> String {
        if !self.conflict() {
            return "No conflict detected.".to_string();
        }
        let mut reasons = Vec::new();
        if self.package_installed {
            reasons.push("the opensnitch-ui package is installed");
        }
        if self.autostart_present {
            reasons.push("its autostart entry is present");
        }
        format!(
            "Upstream opensnitch-ui detected ({}). Running both against the same \
             daemon contends for the UI gRPC channel and risks ui.proto version \
             skew. Uninstall opensnitch-ui, or at least remove {}.",
            reasons.join(" and "),
            crate::paths::opensnitch_ui_autostart_path().display()
        )
    }
}

/// Real check: `rpm -q opensnitch-ui` exit status. Never panics or errors
/// out: a missing `rpm` binary (a non-rpm-based host, or this sandbox) just
/// means "can't tell, assume not installed" — this is a best-effort UX
/// nicety, not a security boundary, so silent degradation is correct here.
fn is_package_installed() -> bool {
    std::process::Command::new("rpm")
        .args(["-q", "opensnitch-ui"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn is_autostart_present(path: &Path) -> bool {
    path.is_file()
}

/// Run both real checks and build a report. Called from
/// `SettingsController::refresh_coexistence` off the Qt thread.
pub fn check_coexistence() -> CoexistenceReport {
    CoexistenceReport {
        package_installed: is_package_installed(),
        autostart_present: is_autostart_present(&crate::paths::opensnitch_ui_autostart_path()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn conflict_true_when_package_installed() {
        let r = CoexistenceReport {
            package_installed: true,
            autostart_present: false,
        };
        assert!(r.conflict());
    }

    #[test]
    fn conflict_true_when_autostart_present() {
        let r = CoexistenceReport {
            package_installed: false,
            autostart_present: true,
        };
        assert!(r.conflict());
    }

    #[test]
    fn conflict_false_when_neither() {
        assert!(!CoexistenceReport::default().conflict());
    }

    #[test]
    fn message_mentions_both_reasons_when_both_present() {
        let r = CoexistenceReport {
            package_installed: true,
            autostart_present: true,
        };
        let msg = r.message();
        assert!(msg.contains("package is installed"));
        assert!(msg.contains("autostart entry is present"));
    }

    #[test]
    fn message_is_clean_when_no_conflict() {
        assert_eq!(
            CoexistenceReport::default().message(),
            "No conflict detected."
        );
    }

    #[test]
    fn is_autostart_present_false_for_missing_file() {
        assert!(!is_autostart_present(&PathBuf::from(
            "/nonexistent/path/opensnitch_ui.desktop"
        )));
    }
}
