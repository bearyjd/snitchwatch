use super::*;

/// #35 / PR #166 H1: the system overlay's config dials the system bridge's
/// root-only socket, relative to the daemon's working directory
/// (/run/snitchwatch, set by the drop-in stage.sh stages with it).
/// (Whether OpenSnitch 1.8.0 accepts the absolute unix:/// form is
/// unverified; see docs/packaging/system-bridge-integration.md.)
/// Owner decision 2026-10-25: the packaged default is `allow` (approve all);
/// deny enforcement is an explicit operator opt-in. A deny default with no
/// decision client answering locked the r15 system out of the network.
#[test]
fn system_overlay_config_defaults_to_allow_on_the_bridge_socket() {
    let json = daemon_config("packaging/system/opensnitchd-default-config.json");
    assert_eq!(
        json["DefaultAction"], "allow",
        "the system overlay's daemon config must default to `allow`; deny is \
         an operator opt-in, not a packaged default"
    );
    assert_eq!(json["Server"]["Address"], "unix:opensnitchd.sock");
}

/// #35: the recipe's config dials the same root-only socket as the system
/// overlay's. Without the overlay the daemon (cwd `/`) can only dial
/// `/opensnitchd.sock`, which only root can create: it finds no UI and applies
/// `allow` (owner decision 2026-10-25). No TCP port is ever left to squat.
#[test]
fn recipe_config_uses_the_root_only_unix_address() {
    let json =
        daemon_config("packaging/bluebuild/files/system/etc/opensnitchd/default-config.json");
    assert_eq!(json["Server"]["Address"], "unix:opensnitchd.sock");
    assert_eq!(json["DefaultAction"], "allow");
}

/// eBPF groundwork (docs/superpowers/specs/2026-10-22-ebpf-groundwork.md):
/// both shipped daemon configs pin the module directory the image installs
/// to. `Ebpf.ModulesPath` is a real key (vendor/opensnitch
/// daemon/procmon/ebpf/config.go:7); when set, `LoadEbpfModule` searches only
/// that path (daemon/core/ebpf.go:23-25).
#[test]
fn both_daemon_configs_pin_the_ebpf_modules_path() {
    for rel in [
        "packaging/system/opensnitchd-default-config.json",
        "packaging/bluebuild/files/system/etc/opensnitchd/default-config.json",
    ] {
        let json = daemon_config(rel);
        assert_eq!(json["ProcMonitorMethod"], "ebpf", "{rel}");
        assert_eq!(
            json["Ebpf"]["ModulesPath"], "/usr/lib/opensnitchd/ebpf",
            "{rel}"
        );
    }
}

/// The recipe's config and the overlay's are the same file: they must not
/// drift on any setting.
#[test]
fn the_two_daemon_configs_are_identical() {
    let recipe =
        daemon_config("packaging/bluebuild/files/system/etc/opensnitchd/default-config.json");
    let system = daemon_config("packaging/system/opensnitchd-default-config.json");
    assert_eq!(recipe, system);
}

fn json_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            json_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "json") {
            found.push(path);
        }
    }
}

/// #35: no shipped daemon config names a TCP `Server.Address`, so there is no
/// loopback port for a local process to squat on.
#[test]
fn no_shipped_daemon_config_names_a_tcp_server_address() {
    let mut found = Vec::new();
    json_files(&workspace_file("packaging"), &mut found);
    let mut configs = 0;
    for path in found {
        let Ok(json) =
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
        else {
            continue;
        };
        let Some(address) = json["Server"]["Address"].as_str() else {
            continue;
        };
        configs += 1;
        assert!(
            address.starts_with("unix:"),
            "{} names a non-unix Server.Address `{address}`",
            path.display()
        );
    }
    assert_eq!(configs, 2, "expected the recipe and system daemon configs");
    let stage = read("packaging/system/stage.sh");
    for staged in [
        "opensnitch-snitchwatch-system-bridge.conf",
        "snitchwatch-system-bridge-grpc.socket",
    ] {
        assert!(stage.contains(staged), "stage.sh doesn't install {staged}");
    }
}

fn files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(&path, found);
        } else {
            found.push(path);
        }
    }
}

/// #35: TCP gRPC is a developer-only transport. Nothing packaged sets the
/// flag or the bind address.
#[test]
fn nothing_under_packaging_sets_the_dev_tcp_knobs() {
    let mut found = Vec::new();
    files_under(&workspace_file("packaging"), &mut found);
    assert!(
        found.len() > 20,
        "scan found too few files: {}",
        found.len()
    );
    for path in found {
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue; // binary fixture
        };
        for knob in ["SNITCHWATCH_GRPC_BIND", "SNITCHWATCH_DEV_TCP"] {
            assert!(
                !body.contains(knob),
                "{} mentions {knob}: packaging must not set the dev TCP transport",
                path.display()
            );
        }
    }
}

#[test]
fn flatpak_manifest_grants_no_network() {
    let body = read("packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml");
    assert!(
        !std::path::Path::new(&workspace_file(
            "packaging/flatpak/org.snitchwatch.Snitchwatch.yml"
        ))
        .exists(),
        "the per-user Flatpak profile is retired (#35); the system profile ships"
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

/// App icons need read-only views of host .desktop files and icon themes.
/// Every such grant must be `:ro`, and the broad `host-os`/`host`/`home`
/// grants must stay out: system-package apps get the generic icon instead.
#[test]
fn flatpak_icon_grants_are_read_only_and_narrow() {
    for path in ["packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml"] {
        let body = read(path);
        let finish_args = body
            .split("finish-args:")
            .nth(1)
            .and_then(|rest| rest.split("\nmodules:").next())
            .expect("manifest must have a finish-args block before modules");
        let args: Vec<&str> = finish_args
            .lines()
            .filter_map(|line| line.trim().strip_prefix("- "))
            .map(str::trim)
            .collect();
        for needed in [
            "--filesystem=/var/lib/flatpak/exports/share:ro",
            "--filesystem=xdg-data/flatpak/exports/share:ro",
            "--filesystem=xdg-data/applications:ro",
            "--filesystem=xdg-data/icons:ro",
        ] {
            assert!(
                args.contains(&needed),
                "{path}: finish-args missing `{needed}`"
            );
        }
        for arg in &args {
            if let Some(target) = arg.strip_prefix("--filesystem=") {
                let broad = matches!(
                    target.split(':').next(),
                    Some("host" | "host-os" | "host-etc" | "home" | "/" | "/usr" | "/var")
                );
                assert!(!broad, "{path}: `{arg}` is too broad for app-icon lookup");
            }
        }
        for arg in args
            .iter()
            .filter(|a| a.contains("applications") || a.contains("icons"))
        {
            assert!(arg.ends_with(":ro"), "{path}: `{arg}` must be read-only");
        }
    }
}

/// The markdown section under `heading` (exact line), up to the next heading
/// of the same or a higher level.
fn doc_section(body: &str, heading: &str) -> String {
    let level = heading.chars().take_while(|c| *c == '#').count();
    let mut lines = body.lines().skip_while(|line| *line != heading);
    assert!(lines.next().is_some(), "heading {heading:?} not found");
    lines
        .take_while(|line| {
            let hashes = line.chars().take_while(|c| *c == '#').count();
            hashes == 0 || hashes > level || !line[hashes..].starts_with(' ')
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Adversarial review of #224: the deny opt-in must be a procedure a user
/// can follow without locking themselves out. Editing the file alone leaves
/// a connected daemon on its cached fallback (`clientConnectedRule`,
/// vendor/opensnitch/daemon/ui/config_utils.go), the fetch rule covers only
/// TCP 443 (not DNS), and recovery must be able to undo `deny` itself.
#[test]
fn the_deny_opt_in_restarts_the_daemon_and_has_a_way_back() {
    let readme = doc_section(
        &read("README.md"),
        "### Default action: `allow` (deny is an opt-in)",
    );
    for needle in [
        "sudo systemctl restart opensnitch.service",
        "127.0.0.53",
        "#restoring-the-allow-default",
        "only to roll back",
    ] {
        assert!(
            readme.contains(needle),
            "README deny opt-in lacks {needle:?}"
        );
    }
    // The way back is prepared before deny is set, and is not a step to run.
    let link = readme.find("#restoring-the-allow-default").unwrap();
    let set_deny = readme
        .find("Set `DefaultAction` to `deny`")
        .expect("README opt-in names the deny step");
    assert!(link < set_deny, "prepare the rollback before setting deny");

    let integration = read("docs/packaging/system-bridge-integration.md");
    let back = doc_section(&integration, "#### Restoring the `allow` default");
    // One root shell builds and validates the replacement, then swaps it in;
    // the restart only runs if that succeeded. A pipeline into `sudo tee`
    // truncates the live file when an unprivileged read of the 0600 config
    // fails (vendor config.go Save()).
    for needle in [
        "sudo sh -c",
        r#"jq -e ".DefaultAction = \"allow\"" "$f" > "$f.new""#,
        r#"mv "$f.new" "$f"'"#,
        "&& sudo systemctl restart opensnitch.service",
    ] {
        assert!(back.contains(needle), "allow restore lacks {needle:?}");
    }
    assert!(
        !back.contains("| sudo tee"),
        "allow restore must not pipe into sudo tee"
    );

    let merge = doc_section(
        &integration,
        "### Before rebooting into the image: the `/etc` merge",
    );
    assert!(
        merge.contains("sudo jq .DefaultAction /etc/opensnitchd/default-config.json"),
        "the /etc merge check must cover DefaultAction, not only Server.Address"
    );
    // The daemon saves this file root-only (0600): unprivileged reads fail.
    let unprivileged = merge.lines().find(|l| l.trim_start().starts_with("jq "));
    assert!(
        unprivileged.is_none(),
        "/etc merge check reads the 0600 config without sudo: {unprivileged:?}"
    );
}

/// Every `install -D… <src> <dest>` source in the Flatpak manifest must exist
/// in the tree. A moved or deleted file (the icon once lived under the
/// removed `web/`) otherwise only fails in a real Flatpak build, which CI
/// cannot run. `target/` and `bin/` sources are build outputs, not tree files.
#[test]
fn flatpak_install_sources_exist_in_the_tree() {
    let body = read("packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml");
    // Fold YAML continuation lines (indented more than the `- install` item)
    // so `install -Dm644 src\n    dest` is read as one command.
    let mut commands: Vec<String> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- install -D") {
            commands.push(trimmed.trim_start_matches("- ").to_string());
        } else if let Some(last) = commands.last_mut() {
            if !trimmed.starts_with('-')
                && !trimmed.starts_with('#')
                && !trimmed.is_empty()
                && !last.contains(" /app/")
            {
                last.push(' ');
                last.push_str(trimmed);
            }
        }
    }
    assert!(
        commands.len() >= 4,
        "found too few install commands: {commands:?}"
    );
    let mut checked = 0;
    for command in commands {
        let src = command
            .split_whitespace()
            .skip(1)
            .find(|t| !t.starts_with('-'))
            .unwrap_or_else(|| panic!("no source in `{command}`"));
        if src.starts_with("target/") || src.starts_with("bin/") {
            continue;
        }
        checked += 1;
        assert!(
            workspace_file(src).is_file(),
            "Flatpak manifest installs `{src}`, which is not in the tree"
        );
    }
    assert!(checked >= 3, "checked too few sources: {checked}");
}
