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
