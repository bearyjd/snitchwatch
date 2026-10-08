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

// --- The packaged fetch rule ---------------------------------------------
//
// The one opensnitchd rule Snitchwatch ships (owner decision 2026-10-08,
// docs/superpowers/plans/2026-10-08-packaged-bridge-fetch-rule.md): the
// system bridge may make HTTPS connections, nothing else. Checked against
// opensnitchd v1.8.0's loader by reading it (`vendor/opensnitch/daemon/rule`);
// no Go toolchain runs it here.

const FETCH_RULE_PATH: &str =
    "packaging/bluebuild/files/system/etc/opensnitchd/rules/000-snitchwatch-bridge-fetch.json";
const BRIDGE_PATH: &str = "/usr/bin/snitchwatch-bridge-cli";
const BRIDGE_ACCOUNT: &str = "snitchwatch";

fn fetch_rule() -> serde_json::Value {
    serde_json::from_str(&read(FETCH_RULE_PATH))
        .expect("the packaged fetch rule must be valid JSON")
}

fn object_keys(value: &serde_json::Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

#[test]
fn packaged_fetch_rule_has_the_owner_approved_shape() {
    let rule = fetch_rule();
    // Go's decoder silently drops a misspelt key, leaving that field false.
    assert_eq!(
        object_keys(&rule),
        [
            "action",
            "created",
            "description",
            "duration",
            "enabled",
            "name",
            "nolog",
            "operator",
            "precedence",
            "updated"
        ]
    );
    // The loader keys the rule by `name` but deletes `<name>.json`.
    let stem = Path::new(FETCH_RULE_PATH).file_stem().unwrap();
    assert_eq!(rule["name"], stem.to_str().unwrap());
    assert_eq!(rule["action"], "allow");
    assert_eq!(rule["duration"], "always");
    assert_eq!(rule["enabled"], true);
    assert_eq!(
        rule["precedence"], false,
        "denies and blocklists must keep winning over this allow"
    );
    assert_eq!(rule["nolog"], false, "the bridge's fetches stay in the log");
    // `Serialize` parses `created` as RFC3339 and warns on every Subscribe if it can't.
    for key in ["created", "updated"] {
        let value = rule[key].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap_or_else(|e| panic!("{key} {value:?} is not RFC3339: {e}"));
    }
    assert_ne!(
        rule["description"], "snitchwatch interactive verdict",
        "a prompt rule's description would let #44 flag it"
    );

    let op = &rule["operator"];
    assert_eq!(
        object_keys(op),
        ["data", "list", "operand", "sensitive", "type"]
    );
    assert_eq!(op["type"], "list");
    assert_eq!(op["operand"], "list");
    assert_eq!(op["data"], "");
    let mut members: Vec<(String, String, String, bool)> = op["list"]
        .as_array()
        .expect("an enabled list rule's members must be in `list` (data isn't read)")
        .iter()
        .map(|m| {
            assert_eq!(
                object_keys(m),
                ["data", "list", "operand", "sensitive", "type"]
            );
            // `Deserialize` copies one level only; a nested list arrives empty.
            assert_eq!(m["list"], serde_json::json!([]), "{m}");
            let text = |key: &str| m[key].as_str().unwrap().to_string();
            (
                text("type"),
                text("operand"),
                text("data"),
                m["sensitive"].as_bool().unwrap(),
            )
        })
        .collect();
    members.sort();
    let mut expected: Vec<(String, String, String, bool)> = [
        ("simple", "process.path", BRIDGE_PATH, true),
        ("simple", "user.name", BRIDGE_ACCOUNT, false),
        ("simple", "dest.port", "443", false),
        ("regexp", "protocol", "^tcp6?$", false),
    ]
    .into_iter()
    .map(|(t, o, d, s)| (t.into(), o.into(), d.into(), s))
    .collect();
    expected.sort();
    assert_eq!(members, expected);
}

/// A connection as opensnitchd's `Match` sees it.
#[derive(Clone, Copy, Debug)]
struct Conn {
    path: &'static str,
    uid: u32,
    port: u16,
    proto: &'static str,
}

/// opensnitchd v1.8.0's verdict for one list member (`operator.go`
/// `Compile` and `Match`, for the operands and types the packaged rule
/// uses). `None`: the member doesn't compile, so the loader skips the rule.
fn member_matches(
    member: &serde_json::Value,
    conn: Conn,
    uid_of: fn(&str) -> Option<u32>,
) -> Option<bool> {
    let text = |key: &str| member[key].as_str().unwrap();
    let sensitive = member["sensitive"].as_bool().unwrap();
    let mut data = text("data").to_string();
    if text("type") == "simple" && text("operand") == "user.name" {
        // `user.Lookup` at compile time; the uid replaces the name.
        data = uid_of(&data)?.to_string();
    }
    let value = match text("operand") {
        "process.path" => conn.path.to_string(),
        "user.name" | "user.id" => conn.uid.to_string(),
        "dest.port" => conn.port.to_string(),
        "protocol" => conn.proto.to_string(),
        other => panic!("this evaluator doesn't model the {other} operand"),
    };
    Some(match text("type") {
        "simple" if sensitive => value == data,
        // `strings.EqualFold`; every value here is ASCII.
        "simple" => value.eq_ignore_ascii_case(&data),
        "regexp" => {
            let (data, value) = if sensitive {
                (data, value)
            } else {
                (data.to_lowercase(), value.to_lowercase())
            };
            regex::Regex::new(&data).unwrap().is_match(&value)
        }
        other => panic!("this evaluator doesn't model the {other} type"),
    })
}

/// The rule's verdict: `listMatch` ANDs the members from `true` (so an empty
/// list matches everything); `None` when the rule doesn't load at all.
fn rule_matches(
    rule: &serde_json::Value,
    conn: Conn,
    uid_of: fn(&str) -> Option<u32>,
) -> Option<bool> {
    let mut matched = true;
    for member in rule["operator"]["list"].as_array().unwrap() {
        matched &= member_matches(member, conn, uid_of)?;
    }
    Some(matched)
}

#[test]
fn packaged_fetch_rule_matches_only_the_system_bridges_https() {
    const UID: u32 = 978;
    fn uid_of(name: &str) -> Option<u32> {
        (name == BRIDGE_ACCOUNT).then_some(UID)
    }
    let rule = fetch_rule();
    let bridge = Conn {
        path: BRIDGE_PATH,
        uid: UID,
        port: 443,
        proto: "tcp",
    };
    // `parseDirection` names IPv6 TCP "tcp6".
    for proto in ["tcp", "tcp6"] {
        let conn = Conn { proto, ..bridge };
        assert_eq!(rule_matches(&rule, conn, uid_of), Some(true), "{conn:?}");
    }
    // One difference each: every condition must narrow the rule.
    for (what, conn) in [
        (
            "another program",
            Conn {
                path: "/usr/bin/curl",
                ..bridge
            },
        ),
        (
            "the bridge from another path",
            Conn {
                path: "/usr/local/bin/snitchwatch-bridge-cli",
                ..bridge
            },
        ),
        (
            "the path in another case",
            Conn {
                path: "/usr/bin/SNITCHWATCH-BRIDGE-CLI",
                ..bridge
            },
        ),
        (
            "a desktop user's bridge",
            Conn {
                uid: 1000,
                ..bridge
            },
        ),
        ("root's bridge", Conn { uid: 0, ..bridge }),
        ("plain HTTP", Conn { port: 80, ..bridge }),
        (
            "HTTPS on another port",
            Conn {
                port: 8443,
                ..bridge
            },
        ),
        (
            "QUIC",
            Conn {
                proto: "udp",
                ..bridge
            },
        ),
        (
            "QUIC over IPv6",
            Conn {
                proto: "udp6",
                ..bridge
            },
        ),
        (
            "SCTP",
            Conn {
                proto: "sctp",
                ..bridge
            },
        ),
    ] {
        assert_eq!(
            rule_matches(&rule, conn, uid_of),
            Some(false),
            "{what}: {conn:?}"
        );
    }
    // Before sysusers creates the account the rule doesn't load at all
    // (`loadRule` returns the compile error): never broader.
    assert_eq!(rule_matches(&rule, bridge, |_| None), None);
}

/// `fetch_rule`'s operator as opensnitchd's `Serialize` reports it: with
/// `compiled_uid`, the `user.name` member holds that uid, as `Compile` leaves
/// it for an enabled rule.
fn fetch_rule_operator(compiled_uid: Option<&str>) -> snitchwatch_proto::protocol::Operator {
    use snitchwatch_proto::protocol::Operator;
    let rule = fetch_rule();
    let leaf = |m: &serde_json::Value| {
        let text = |key: &str| m[key].as_str().unwrap().to_string();
        Operator {
            r#type: text("type"),
            operand: text("operand"),
            data: match (compiled_uid, m["operand"].as_str()) {
                (Some(uid), Some("user.name")) => uid.to_string(),
                _ => text("data"),
            },
            sensitive: m["sensitive"].as_bool().unwrap(),
            list: Vec::new(),
        }
    };
    Operator {
        r#type: "list".into(),
        operand: "list".into(),
        list: rule["operator"]["list"]
            .as_array()
            .unwrap()
            .iter()
            .map(leaf)
            .collect(),
        ..Default::default()
    }
}

#[test]
fn packaged_fetch_rule_passes_the_bridges_rule_checks() {
    use snitchwatch_bridge::{rule_name, rule_policy};
    let rule = fetch_rule();
    let name = rule["name"].as_str().unwrap();
    rule_name::validate_rule_name(name).unwrap();
    // Reserved, so no GUI action or import can replace, re-duration or
    // delete it, and in no band the bridge manages (and purges) itself.
    assert_eq!(name, rule_name::PACKAGED_FETCH_RULE_NAME);
    assert!(rule_name::is_reserved_packaged_name(name));
    assert!(!rule_name::is_reserved_blocklist_name(name));
    let profile_band = snitchwatch_bridge::profiles::materializer::PROFILE_BAND_PREFIX;
    for prefix in ["snitchwatch-default-", profile_band] {
        assert!(!name.starts_with(prefix), "{name} is under {prefix}");
    }
    let daemon_rule = snitchwatch_proto::protocol::Rule {
        name: name.to_string(),
        operator: Some(fetch_rule_operator(Some("978"))),
        ..Default::default()
    };
    assert_eq!(
        rule_policy::read_only_reason(&daemon_rule),
        Some(rule_policy::PACKAGED_FETCH_RULE_REASON)
    );
    assert!(!rule_policy::deletable(&daemon_rule));
    // The conditions themselves are ones the daemon evaluates as written;
    // its compiled form (the uid in user.name) could never be sent back.
    rule_policy::validate_operator(&fetch_rule_operator(None)).unwrap();
    assert!(rule_policy::validate_operator(&fetch_rule_operator(Some("978"))).is_err());
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
