//! Contracts for the socket-activated system-bridge overlay.
//!
//! Most are file-shape tests; the stager tests execute its safe, temporary
//! image-root path. CI does not own a system manager, so a real Bazzite VM
//! still verifies activation and Flatpak group handling.

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn file(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn active_lines(body: &str) -> Vec<&str> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
        .collect()
}

#[test]
fn runtime_parents_are_root_owned_and_not_replaceable() {
    let body = file("packaging/system/snitchwatch.conf");
    let lines = active_lines(&body);
    assert!(lines.contains(&"d /run/snitchwatch 0711 root root -"));
    assert!(lines.contains(&"d /run/snitchwatch-auth 2750 snitchwatch snitchwatch-ui -"));
}

#[test]
fn system_sockets_have_distinct_named_privilege_boundaries() {
    let grpc_body = file("packaging/system/snitchwatch-system-bridge-grpc.socket");
    let grpc = active_lines(&grpc_body);
    for expected in [
        "ListenStream=/run/snitchwatch/opensnitchd.sock",
        "SocketUser=root",
        "SocketGroup=root",
        "SocketMode=0600",
        "FileDescriptorName=grpc",
        "Service=snitchwatch-system-bridge.service",
        "Accept=no",
        "WantedBy=sockets.target",
    ] {
        assert!(grpc.contains(&expected), "gRPC socket missing {expected}");
    }

    let gui_body = file("packaging/system/snitchwatch-system-bridge-gui.socket");
    let gui = active_lines(&gui_body);
    for expected in [
        "ListenStream=/run/snitchwatch/bridge.sock",
        "SocketUser=root",
        "SocketGroup=snitchwatch-ui",
        "SocketMode=0660",
        "FileDescriptorName=gui",
        "Service=snitchwatch-system-bridge.service",
        "Accept=no",
        "WantedBy=sockets.target",
    ] {
        assert!(gui.contains(&expected), "GUI socket missing {expected}");
    }
}

#[test]
fn service_is_unprivileged_socket_activation_only() {
    let service_body = file("packaging/system/snitchwatch-system-bridge.service");
    let lines = active_lines(&service_body);
    for expected in [
        "User=snitchwatch",
        "Group=snitchwatch",
        "Environment=SNITCHWATCH_SYSTEM_BRIDGE=1",
        "Environment=SNITCHWATCH_WS_SOCKET=/run/snitchwatch/bridge.sock",
        "Environment=SNITCHWATCH_WS_TOKEN_PATH=/run/snitchwatch-auth/token",
        "Environment=HOME=/var/lib/snitchwatch",
        "Environment=XDG_STATE_HOME=/var/lib",
        "Sockets=snitchwatch-system-bridge-grpc.socket snitchwatch-system-bridge-gui.socket",
        "StateDirectory=snitchwatch",
        "StateDirectoryMode=0700",
        "NoNewPrivileges=true",
        "CapabilityBoundingSet=",
        "ProtectSystem=strict",
        "ProtectHome=true",
        "PrivateTmp=true",
        "PrivateDevices=true",
        "ProtectKernelTunables=true",
        "ProtectKernelModules=true",
        "ProtectControlGroups=true",
        "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
        "ReadWritePaths=/run/snitchwatch-auth",
    ] {
        assert!(lines.contains(&expected), "service missing {expected}");
    }
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("SNITCHWATCH_GRPC_BIND")),
        "the system service must not retain a TCP gRPC fallback"
    );
}

/// Issue #45: both bridge units cap the bridge's memory, so a hostile or
/// oversized blocklist can't take the desktop down with it.
#[test]
fn both_bridge_units_cap_memory() {
    for unit in [
        "packaging/system/snitchwatch-system-bridge.service",
        "packaging/systemd/snitchwatch-bridge.service",
    ] {
        let body = file(unit);
        let lines = active_lines(&body);
        assert!(
            lines.contains(&"MemoryMax=512M"),
            "{unit} missing MemoryMax=512M"
        );
    }
}

#[test]
fn sysusers_and_stager_keep_enrollment_and_install_offline() {
    let users_body = file("packaging/system/snitchwatch.conf.sysusers");
    let users = active_lines(&users_body);
    for expected in [
        "g snitchwatch -",
        "g snitchwatch-ui -",
        "u snitchwatch -:snitchwatch \"Snitchwatch bridge service account\" /var/lib/snitchwatch",
    ] {
        assert!(
            users.contains(&expected),
            "sysusers file missing {expected}"
        );
    }

    let stage = file("packaging/system/stage.sh");
    for expected in [
        "usage: $0 DEST BINARY SHA256",
        "DEST must be a staging root, not /",
        "cannot resolve BINARY",
        "BINARY SHA256 does not match the verified value",
        "SNITCHWATCH_SYSTEM_BRIDGE=1",
        "usr/lib/systemd/system/snitchwatch-system-bridge.service",
        "usr/lib/tmpfiles.d/snitchwatch.conf",
        "usr/lib/sysusers.d/snitchwatch.conf",
    ] {
        assert!(stage.contains(expected), "stage.sh missing {expected}");
    }
    for forbidden in ["systemctl", "sudo", "rpm-ostree", "flatpak"] {
        assert!(
            !stage.contains(forbidden),
            "stage.sh must not mutate live system state ({forbidden})"
        );
    }
}

fn stage_script() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/system/stage.sh")
}

#[test]
fn stager_rejects_root_after_canonicalization_before_running_binary() {
    let dir = tempdir().unwrap();
    let root_link = dir.path().join("root-link");
    symlink("/", &root_link).unwrap();

    for alias in ["/", "/.", "/tmp/../", root_link.to_str().unwrap()] {
        let result = Command::new("bash")
            .arg(stage_script())
            .arg(alias)
            // This deliberately is not executable: the canonical root guard
            // must run before a binary is inspected or invoked.
            .arg(dir.path().join("not-a-binary"))
            .arg("0".repeat(64))
            .output()
            .unwrap();
        assert!(!result.status.success(), "root alias {alias} must fail");
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("DEST must be a staging root, not /"),
            "root alias {alias} failed for the wrong reason: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn stager_accepts_a_canonical_nonroot_destination_and_only_stages_files() {
    let dir = tempdir().unwrap();
    let binary = dir.path().join("new bridge binary");
    std::fs::write(
        &binary,
        "#!/bin/sh\nif [ \"$1\" = --help ]; then echo SNITCHWATCH_SYSTEM_BRIDGE=1; exit 0; fi\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let digest = format!("{:x}", Sha256::digest(std::fs::read(&binary).unwrap()));
    let destination = dir.path().join("image/../image-root");

    let result = Command::new("bash")
        .arg(stage_script())
        .arg(&destination)
        .arg(&binary)
        .arg(digest)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stage should succeed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let root = dir.path().join("image-root");
    assert!(root.join("usr/bin/snitchwatch-bridge-cli").is_file());
    assert!(root
        .join("usr/lib/systemd/system/snitchwatch-system-bridge.service")
        .is_file());
    assert!(root.join("usr/lib/tmpfiles.d/snitchwatch.conf").is_file());
    assert!(root.join("usr/lib/sysusers.d/snitchwatch.conf").is_file());
}

#[test]
fn flatpak_mounts_only_the_system_bridge_paths_read_only() {
    let manifest = file("packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml");
    assert!(manifest.contains("--filesystem=/run/snitchwatch:ro"));
    assert!(manifest.contains("--filesystem=/run/snitchwatch-auth:ro"));
    assert!(manifest.contains("--env=SNITCHWATCH_SYSTEM_BRIDGE=1"));
    assert!(manifest.contains("same app-id"));
    assert!(!manifest.contains("--filesystem=xdg-run/snitchwatch"));

    let legacy = file("packaging/flatpak/org.snitchwatch.Snitchwatch.yml");
    assert!(legacy.contains("--filesystem=xdg-run/snitchwatch"));
    assert!(!legacy.contains("--env=SNITCHWATCH_SYSTEM_BRIDGE=1"));
}
