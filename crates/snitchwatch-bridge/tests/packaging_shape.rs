//! Shape tests for the Phase 2 packaging artifacts.
//!
//! These do NOT invoke bluebuild / flatpak-builder / systemd — that needs a
//! real Bazzite host the CI sandbox lacks. They assert the load-bearing
//! invariants of the on-disk artifacts so a careless edit can't silently
//! regress them: the fail-closed daemon default, the daemon's dial-in
//! address, and — most importantly — that the Flatpak manifest grants the
//! Unix-socket filesystem permission and does NOT grant network access.

use std::path::{Path, PathBuf};

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

#[test]
fn daemon_config_fails_closed_and_dials_bridge_default_bind() {
    let body = read("packaging/bluebuild/files/system/etc/opensnitchd/default-config.json");
    let json: serde_json::Value =
        serde_json::from_str(&body).expect("daemon config must be valid JSON");

    assert_eq!(
        json["DefaultAction"], "deny",
        "packaged daemon config must fail CLOSED (DefaultAction: deny), not \
         inherit upstream's fail-open `allow`"
    );
    assert_eq!(
        json["Server"]["Address"], "127.0.0.1:50051",
        "Server.Address must point at the bridge's default gRPC bind"
    );
}

#[test]
fn flatpak_manifest_grants_socket_filesystem_but_not_network() {
    let body = read("packaging/flatpak/org.snitchwatch.Snitchwatch.yml");

    assert!(
        body.contains("--filesystem=xdg-run/snitchwatch"),
        "Flatpak manifest must grant --filesystem=xdg-run/snitchwatch to reach \
         the host-side bridge's Unix socket + token"
    );

    // The critical negative invariant. --share=network is allowed in
    // build-args (build-time crate linking) but must NEVER appear in
    // finish-args (the runtime sandbox grant). Assert no finish-args *list
    // item* (a `- <arg>` line, ignoring comments/prose) is `--share=network`.
    let finish_args = body
        .split("finish-args:")
        .nth(1)
        .expect("manifest must have a finish-args block")
        .split("\nmodules:")
        .next()
        .expect("finish-args block must be followed by modules");
    let grants_network = finish_args.lines().any(|line| {
        let trimmed = line.trim();
        // A YAML list entry is `- <value>`; comments start with `#`.
        trimmed
            .strip_prefix("- ")
            .map(|arg| arg.trim() == "--share=network")
            .unwrap_or(false)
    });
    assert!(
        !grants_network,
        "Flatpak finish-args must NOT grant --share=network — a Flatpak's \
         private network namespace can't reach host loopback anyway, and the \
         grant would open full internet access. finish-args block was:\n{finish_args}"
    );
}

#[test]
fn bridge_user_unit_pins_stable_grpc_bind_and_is_a_user_service() {
    let body = read("packaging/systemd/snitchwatch-bridge.service");
    for needle in [
        "[Service]",
        "ExecStart=",
        "snitchwatch-bridge-cli",
        "SNITCHWATCH_GRPC_BIND=127.0.0.1:50051",
        "KillSignal=SIGTERM",
        "WantedBy=default.target",
    ] {
        assert!(
            body.contains(needle),
            "bridge user unit missing `{needle}`\nbody:\n{body}"
        );
    }
    // It must be a plain user service, not tied to any GUI window lifecycle.
    assert!(
        !body.contains("multi-user.target"),
        "bridge unit is a --user service; WantedBy should be default.target"
    );
}

/// Split a systemd unit into `(section header, active lines)` pairs, in file
/// order. Comments (`#`/`;`) and blank lines are dropped, so assertions made on
/// the result can't be satisfied (or tripped) by prose in a comment.
fn unit_sections(body: &str) -> Vec<(String, Vec<String>)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for line in body.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            sections.push((line.to_string(), Vec::new()));
        } else {
            let (_, lines) = sections
                .last_mut()
                .unwrap_or_else(|| panic!("unit line outside any section: `{line}`"));
            lines.push(line.to_string());
        }
    }
    sections
}

/// The active lines of every section called `header` (e.g. `[Unit]`).
fn section_lines(sections: &[(String, Vec<String>)], header: &str) -> Vec<String> {
    sections
        .iter()
        .filter(|(name, _)| name == header)
        .flat_map(|(_, lines)| lines.iter().cloned())
        .collect()
}

#[test]
fn unit_section_splitter_ignores_comments_and_attributes_lines_to_sections() {
    let sections = unit_sections(
        "# header comment\n[Unit]\nA=1\n; also a comment\n\n[Service]\n  B=2  \n# C=3\n[Install]\nD=4\n",
    );
    assert_eq!(section_lines(&sections, "[Unit]"), ["A=1"]);
    assert_eq!(section_lines(&sections, "[Service]"), ["B=2"]);
    assert_eq!(section_lines(&sections, "[Install]"), ["D=4"]);
    assert!(section_lines(&sections, "[Missing]").is_empty());
}

/// The release artifact installs this exact file to
/// `/usr/lib/systemd/user/` in an immutable image and enables it with
/// `systemctl --global enable`, so it must not depend on anything under a
/// user's home and must not start for system accounts.
#[test]
fn bridge_user_unit_matches_the_image_baked_contract() {
    let body = read("packaging/systemd/snitchwatch-bridge.service");
    let sections = unit_sections(&body);

    let exec_starts: Vec<String> = section_lines(&sections, "[Service]")
        .into_iter()
        .filter(|line| line.starts_with("ExecStart"))
        .collect();
    assert_eq!(
        exec_starts,
        ["ExecStart=/usr/bin/snitchwatch-bridge-cli"],
        "[Service] must have exactly one ExecStart, at the image-baked path"
    );

    for (header, lines) in &sections {
        for line in lines {
            for forbidden in ["%h", "~", ".local"] {
                assert!(
                    !line.contains(forbidden),
                    "{header} line `{line}` contains `{forbidden}`: an image-baked \
                     unit must not reference a home directory"
                );
            }
        }
    }

    assert!(
        section_lines(&sections, "[Unit]")
            .iter()
            .any(|line| line == "ConditionUser=!@system"),
        "[Unit] must carry `ConditionUser=!@system` so a globally-enabled unit \
         never starts for system accounts (e.g. a display-manager greeter)\nbody:\n{body}"
    );

    assert!(
        section_lines(&sections, "[Install]")
            .iter()
            .any(|line| line == "WantedBy=default.target"),
        "[Install] must carry `WantedBy=default.target`\nbody:\n{body}"
    );
}

#[test]
fn bluebuild_recipe_installs_and_enables_opensnitchd() {
    let body = read("packaging/bluebuild/recipe.yml");
    for needle in [
        "base-image: ghcr.io/ublue-os/bazzite",
        "type: rpm-ostree",
        "- opensnitch",
        "type: files",
        "type: systemd",
        "- opensnitchd.service",
    ] {
        assert!(
            body.contains(needle),
            "bluebuild recipe missing `{needle}`\nbody:\n{body}"
        );
    }
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
