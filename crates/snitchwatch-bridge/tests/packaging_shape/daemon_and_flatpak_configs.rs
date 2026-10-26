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

/// PR #166 H1: the recipe's own `files` module ships without the overlay,
/// so its config must not dial the socket. It keeps the per-user bridge's
/// TCP address and, like the overlay, defaults to `allow` (owner decision
/// 2026-10-25); the squatter case (#35) is what the system overlay's socket
/// fixes.
#[test]
fn recipe_config_defaults_to_allow_on_the_per_user_tcp_bind() {
    let json =
        daemon_config("packaging/bluebuild/files/system/etc/opensnitchd/default-config.json");
    assert_eq!(json["Server"]["Address"], "127.0.0.1:50051");
    assert_eq!(json["DefaultAction"], "allow");
}

/// The two daemon configs differ only in where the daemon dials: the
/// overlay's must not drift from the recipe's on any other setting.
#[test]
fn the_two_daemon_configs_differ_only_in_server_address() {
    let mut recipe =
        daemon_config("packaging/bluebuild/files/system/etc/opensnitchd/default-config.json");
    let mut system = daemon_config("packaging/system/opensnitchd-default-config.json");
    assert_ne!(recipe["Server"]["Address"], system["Server"]["Address"]);
    recipe["Server"]["Address"] = serde_json::Value::Null;
    system["Server"]["Address"] = serde_json::Value::Null;
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

/// PR #166 H1: a `unix:` Server.Address ships only in the staged system
/// overlay, which installs it with the drop-in and the socket it dials, as
/// one artifact. Nothing else under packaging/ (the recipe included) may.
#[test]
fn a_unix_server_address_ships_only_with_the_socket_and_drop_in() {
    let mut found = Vec::new();
    json_files(&workspace_file("packaging"), &mut found);
    let system = workspace_file("packaging/system").canonicalize().unwrap();
    let stage = read("packaging/system/stage.sh");
    let mut unix_configs = 0;
    for path in found {
        let Ok(json) =
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
        else {
            continue;
        };
        let address = json["Server"]["Address"].as_str().unwrap_or_default();
        if !address.starts_with("unix:") {
            continue;
        }
        unix_configs += 1;
        let path = path.canonicalize().unwrap();
        assert!(
            path.starts_with(&system),
            "{} ships a unix: Server.Address outside the staged overlay",
            path.display()
        );
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(stage.contains(name), "stage.sh doesn't install {name}");
    }
    assert_eq!(unix_configs, 1);
    for staged in [
        "opensnitch-snitchwatch-system-bridge.conf",
        "snitchwatch-system-bridge-grpc.socket",
    ] {
        assert!(stage.contains(staged), "stage.sh doesn't install {staged}");
    }
    let recipe = read("packaging/bluebuild/recipe.yml");
    for line in recipe.lines().filter(|l| !l.trim_start().starts_with('#')) {
        assert!(!line.contains("unix:"), "recipe.yml line: {line}");
    }
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

/// App icons need read-only views of host .desktop files and icon themes.
/// Every such grant must be `:ro`, and the broad `host-os`/`host`/`home`
/// grants must stay out: system-package apps get the generic icon instead.
#[test]
fn flatpak_icon_grants_are_read_only_and_narrow() {
    for path in [
        "packaging/flatpak/org.snitchwatch.Snitchwatch.yml",
        "packaging/flatpak/org.snitchwatch.Snitchwatch.system.yml",
    ] {
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
    ] {
        assert!(
            readme.contains(needle),
            "README deny opt-in lacks {needle:?}"
        );
    }
    let integration = read("docs/packaging/system-bridge-integration.md");
    let back = doc_section(&integration, "#### Restoring the `allow` default");
    for needle in [
        "DefaultAction = \"allow\"",
        "sudo systemctl restart opensnitch.service",
    ] {
        assert!(back.contains(needle), "allow restore lacks {needle:?}");
    }
    let merge = doc_section(
        &integration,
        "### Before rebooting into the image: the `/etc` merge",
    );
    assert!(
        merge.contains("jq .DefaultAction /etc/opensnitchd/default-config.json"),
        "the /etc merge check must cover DefaultAction, not only Server.Address"
    );
}
