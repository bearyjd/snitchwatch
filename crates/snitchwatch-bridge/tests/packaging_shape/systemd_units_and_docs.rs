use super::*;

/// #35: the per-user bridge unit is gone; only the system units ship.
#[test]
fn the_per_user_bridge_unit_is_not_shipped() {
    assert!(
        !workspace_file("packaging/systemd/snitchwatch-bridge.service").exists(),
        "the per-user unit is retired (#35)"
    );
    assert!(!workspace_file("packaging/systemd").exists());
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

#[test]
fn bluebuild_recipe_installs_and_enables_opensnitchd() {
    let body = read("packaging/bluebuild/recipe.yml");
    for needle in [
        "base-image: ghcr.io/ublue-os/bazzite",
        "type: rpm-ostree",
        "- opensnitch",
        "type: files",
        "type: systemd",
        // The RPM installs the unit as `opensnitch.service` and ships no
        // `opensnitchd.service` alias (`vendor:utils/packaging/daemon/rpm/
        // opensnitch.spec`, issue #92).
        "- opensnitch.service",
    ] {
        assert!(
            body.contains(needle),
            "bluebuild recipe missing `{needle}`\nbody:\n{body}"
        );
    }
    assert!(
        !body.contains("opensnitchd.service"),
        "the RPM has no opensnitchd.service unit (issue #92)"
    );
}

/// The root daemon RPM must be sha256-pinned and verified before install: a
/// bare release URL in the rpm-ostree module trusts whatever GitHub serves.
#[test]
fn bluebuild_recipe_pins_opensnitch_rpm_sha256() {
    let body = read("packaging/bluebuild/recipe.yml");
    let rpm_url = body
        .lines()
        .map(str::trim)
        .find(|l| l.contains("opensnitch-") && l.contains(".rpm"))
        .unwrap_or_else(|| panic!("recipe must reference the opensnitch RPM\n{body}"));
    assert!(
        !rpm_url.starts_with("- https://"),
        "RPM must not be a bare URL in the rpm-ostree install list: {rpm_url}"
    );
    assert!(
        body.contains("sha256sum -c"),
        "recipe must verify the RPM with `sha256sum -c` before install"
    );
    let digest = body
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|w| w.len() == 64)
        .expect("recipe must pin a 64-hex sha256 digest");
    assert!(digest.chars().all(|c| !c.is_ascii_uppercase()));
}

/// Issue #92: the layering guide's commands name the RPM's real unit.
#[test]
fn the_layering_guide_names_the_rpms_unit() {
    let body = read("docs/packaging/rpm-ostree-layering.md");
    assert!(
        body.contains("systemctl enable --now opensnitch.service"),
        "{body}"
    );
    assert!(
        !body.contains("opensnitchd.service"),
        "the RPM has no opensnitchd.service unit (issue #92)"
    );
}

/// Lines of `text` that run `systemctl`/`journalctl` against `opensnitchd`
/// (or `opensnitchd.service`) as the unit name. `opensnitchd` is the daemon's
/// binary; the unit is `opensnitch.service` (issue #92). Paths such as
/// `/etc/opensnitchd/`, units such as `snitchwatch-opensnitchd.service`, and
/// prose that explains the old name without a command don't match.
fn opensnitchd_unit_commands(text: &str) -> Vec<&str> {
    let re = regex::Regex::new(
        r#"\b(systemctl|journalctl)\b.*[\s'"`=]opensnitchd(\.service)?($|[\s'"`;|&)])"#,
    )
    .expect("static regex");
    text.lines().filter(|line| re.is_match(line)).collect()
}

#[test]
fn the_unit_name_matcher_flags_only_commands_naming_opensnitchd() {
    for bad in [
        "systemctl status opensnitchd    # should be enabled + active",
        "sudo systemctl restart opensnitchd",
        "sudo systemctl enable --now opensnitchd.service",
        "journalctl -u opensnitchd -f",
        "journalctl -u opensnitchd.service",
    ] {
        assert!(!opensnitchd_unit_commands(bad).is_empty(), "missed: {bad}");
    }
    for ok in [
        "systemctl status opensnitch.service",
        "systemctl is-active opensnitch.service       # -> active",
        "journalctl -u opensnitch.service",
        "systemctl --user start snitchwatch-opensnitchd.service",
        "sudo systemctl restart opensnitch && cat /etc/opensnitchd/default-config.json",
        "`opensnitchd` is the daemon's binary, not its unit.",
        "`opensnitch.service`, with no `opensnitchd.service` alias (issue #92).",
    ] {
        assert!(opensnitchd_unit_commands(ok).is_empty(), "false hit: {ok}");
    }
}

/// Issue #92: no packaging doc or the recipe tells a reader to run
/// `systemctl`/`journalctl` against the unit that doesn't exist.
#[test]
fn packaging_docs_never_run_systemctl_against_opensnitchd() {
    let mut files = vec![
        "packaging/README.md".to_string(),
        "packaging/bluebuild/recipe.yml".to_string(),
    ];
    let docs = workspace_file("docs/packaging");
    for entry in
        std::fs::read_dir(&docs).unwrap_or_else(|e| panic!("read_dir {}: {e}", docs.display()))
    {
        let name = entry.expect("dir entry").file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".md") {
            files.push(format!("docs/packaging/{name}"));
        }
    }
    // A rename must not quietly turn this scan into a no-op.
    for expected in [
        "phase2-manual-verification-runbook.md",
        "rpm-ostree-layering.md",
    ] {
        assert!(
            files.iter().any(|f| f.ends_with(expected)),
            "{expected} missing from the scan: {files:?}"
        );
    }

    for file in &files {
        let body = read(file);
        let hits = opensnitchd_unit_commands(&body);
        assert!(
            hits.is_empty(),
            "{file} runs systemctl/journalctl against `opensnitchd`; the unit is \
             `opensnitch.service` (issue #92): {hits:?}"
        );
    }
}
