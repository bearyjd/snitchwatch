//! Shape tests for the Phase 2 packaging artifacts.
//!
//! These do NOT invoke bluebuild / flatpak-builder / systemd — that needs a
//! real Bazzite host the CI sandbox lacks. They assert the load-bearing
//! invariants of the on-disk artifacts so a careless edit can't silently
//! regress them: the daemon defaults and dial-in addresses (#35), and — most importantly — that the Flatpak manifest grants the
//! Unix-socket filesystem permission and does NOT grant network access.

use std::path::{Path, PathBuf};

mod daemon_and_flatpak_configs;
mod endpoint_events;
mod fetch_rule;
mod systemd_units_and_docs;

fn workspace_file(rel: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/snitchwatch-bridge; go up two to the root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    let path = workspace_file(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

fn daemon_config(rel: &str) -> serde_json::Value {
    serde_json::from_str(&read(rel)).expect("daemon config must be valid JSON")
}

/// The one license every Snitchwatch-owned declaration must agree on (plan
/// decision G, 2026-10-03). GPL-3.0 because the shipped binaries combine our
/// code with GPL-3.0 `ui.proto`-generated code, Apache-2.0-only crates and
/// (Kirigami) LGPL-3.0 Qt, none of which GPL-2.0-only can combine with.
const PROJECT_LICENSE: &str = "GPL-3.0-or-later";

/// `license = "…"` values (ignoring `license.workspace = true`) in a Cargo.toml.
fn cargo_license_values(rel: &str) -> Vec<String> {
    read(rel)
        .lines()
        .filter_map(|line| line.trim().strip_prefix("license = "))
        .map(|value| value.trim().trim_matches('"').to_string())
        .collect()
}

#[test]
fn project_license_is_declared_consistently() {
    let license = read("LICENSE");
    assert!(
        license.contains("GNU GENERAL PUBLIC LICENSE")
            && license.contains("Version 3, 29 June 2007"),
        "repo-root LICENSE must be the GPL-3.0 text (it ships in the release tarball)"
    );

    assert_eq!(
        cargo_license_values("Cargo.toml"),
        vec![PROJECT_LICENSE],
        "[workspace.package] license"
    );
    // Crates that don't inherit the workspace field must still match it.
    for rel in [
        "crates/snitchwatch-tauri/Cargo.toml",
        "crates/snitchwatch-kirigami/Cargo.toml",
    ] {
        for value in cargo_license_values(rel) {
            assert_eq!(value, PROJECT_LICENSE, "{rel} license");
        }
    }

    let metainfo = read("packaging/flatpak/org.snitchwatch.Snitchwatch.metainfo.xml");
    assert!(
        metainfo.contains(&format!(
            "<project_license>{PROJECT_LICENSE}</project_license>"
        )),
        "Flatpak metainfo project_license must be {PROJECT_LICENSE}"
    );
}
