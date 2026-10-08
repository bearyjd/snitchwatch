//! The data file, the rules it builds and the curated-specific allowlist.

use super::*;
use crate::rule_policy::validate_operator;

fn flatpak() -> CuratedEntry {
    entries()
        .iter()
        .find(|entry| entry.id == "flatpak-flathub")
        .expect("the flatpak entry")
        .clone()
}

#[test]
fn the_data_file_parses_and_offers_the_reviewed_entries() {
    let ids: Vec<&str> = entries().iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "networkmanager-connectivity-check",
            "chronyc-local",
            "flatpak-flathub"
        ]
    );
}

#[test]
fn every_entry_builds_a_narrow_allow_under_the_reserved_prefix() {
    for entry in entries() {
        let rule = entry.rule();
        assert!(
            rule.name.starts_with("snitchwatch-default-"),
            "{}",
            rule.name
        );
        assert!(crate::rule_name::is_reserved_name(&rule.name));
        assert_eq!(rule.description, DESCRIPTION);
        assert_eq!(
            (
                rule.action.as_str(),
                rule.duration.as_str(),
                rule.precedence
            ),
            ("allow", "always", false)
        );
        let op = rule.operator.as_ref().unwrap();
        validate_operator(op).unwrap();
        let path = &op.list[0];
        assert_eq!(
            (path.operand.as_str(), path.r#type.as_str(), path.sensitive),
            ("process.path", "simple", true)
        );
        assert!(path.data.starts_with("/usr/"), "{}", path.data);
        check_curated_rule(&rule).unwrap();
        assert_eq!(entry_for_rule_name(&rule.name), Some(entry));
    }
}

#[test]
fn each_entry_says_exactly_what_it_allows() {
    let allows: Vec<String> = entries().iter().map(CuratedEntry::allows).collect();
    assert_eq!(
        allows,
        [
            "/usr/bin/NetworkManager may connect to fedoraproject.org on TCP port 80, over IPv4 and IPv6.",
            "/usr/bin/chronyc may connect to this computer only (127.0.0.1 and ::1) on UDP port 323, over IPv4 and IPv6.",
            "/usr/bin/flatpak may connect to dl.flathub.org on TCP port 443, over IPv4 and IPv6.",
        ]
    );
}

#[test]
fn the_allowlist_takes_only_the_exact_curated_shape() {
    let base = flatpak().rule();
    let changed = |change: &dyn Fn(&mut Rule)| {
        let mut rule = base.clone();
        change(&mut rule);
        check_curated_rule(&rule)
    };
    let leaf_at = |index: usize, change: &dyn Fn(&mut Operator)| {
        changed(&|rule: &mut Rule| change(&mut rule.operator.as_mut().unwrap().list[index]))
    };
    assert!(changed(&|_| {}).is_ok());
    assert!(changed(&|rule| rule.name = "flatpak-flathub".into()).is_err());
    assert!(changed(&|rule| rule.name = "snitchwatch-default-Bad Id".into()).is_err());
    assert!(changed(&|rule| rule.precedence = true).is_err());
    assert!(changed(&|rule| rule.action = "deny".into()).is_err());
    assert!(changed(&|rule| rule.duration = "until restart".into()).is_err());
    assert!(changed(&|rule| rule.description = "mine".into()).is_err());
    // The program: exact, case sensitive, under /usr.
    assert!(leaf_at(0, &|op| op.data = "/home/u/.local/bin/flatpak".into()).is_err());
    assert!(leaf_at(0, &|op| op.data = "/usr/bin/../bin/flatpak".into()).is_err());
    assert!(leaf_at(0, &|op| op.data = "flatpak".into()).is_err());
    assert!(leaf_at(0, &|op| op.sensitive = false).is_err());
    // A regexp, even one spelling the exact path, matches other programs.
    assert!(leaf_at(0, &|op| op.r#type = "regexp".into()).is_err());
    assert!(leaf_at(0, &|op| {
        op.r#type = "regexp".into();
        op.data = "^/usr/bin/.*$".into();
    })
    .is_err());
    // The host: one plain name, no wildcard.
    assert!(leaf_at(1, &|op| op.data = "*.flathub.org".into()).is_err());
    assert!(leaf_at(1, &|op| {
        op.r#type = "regexp".into();
        op.data = r"\.flathub\.org$".into();
    })
    .is_err());
    // A destination: one host or this computer, never any address.
    assert!(changed(&|rule| {
        rule.operator.as_mut().unwrap().list.remove(1);
    })
    .is_err());
    // The port and the transport.
    assert!(leaf_at(2, &|op| op.data = "0".into()).is_err());
    assert!(leaf_at(2, &|op| op.data = "1-65535".into()).is_err());
    assert!(leaf_at(2, &|op| op.data = "+443".into()).is_err());
    assert!(leaf_at(3, &|op| op.data = "^.*$".into()).is_err());
    // Nothing else.
    assert!(changed(&|rule| {
        let extra = leaf("simple", "process.command", "flatpak update", false);
        rule.operator.as_mut().unwrap().list.push(extra);
    })
    .is_err());
    assert!(changed(&|rule| {
        rule.operator.as_mut().unwrap().list.remove(0);
    })
    .is_err());
}

#[test]
fn a_bad_entry_is_refused() {
    let file = |entry: &str| format!(r#"{{"version": 1, "entries": [{entry}]}}"#);
    let entry = |path: &str, rest: &str| {
        format!(
            r#"{{"id": "x", "path": "{path}", "port": 443, "protocol": "tcp", "why": "w", "evidence": "e"{rest}}}"#
        )
    };
    let host = r#", "host": "a.org""#;
    assert!(parse(&file(&entry("/usr/bin/flatpak", host))).is_ok());
    assert!(parse(&file(&entry("/usr/bin/flatpak", r#", "loopback": true"#))).is_ok());
    for bad in [
        entry("/home/u/bin/flatpak", host),
        entry("/opt/x/flatpak", host),
        entry("/usr/bin/./flatpak", host),
        // No destination: any address is never offered in v1 (S3).
        entry("/usr/bin/flatpak", ""),
        entry("/usr/bin/flatpak", r#", "host": "*.flathub.org""#),
        entry("/usr/bin/flatpak", r#", "host": "10.0.2.3""#),
        entry("/usr/bin/flatpak", r#", "host": "a.org", "loopback": true"#),
        entry(
            "/usr/bin/flatpak",
            r#", "host": "a.org", "pathRegexp": ".*""#,
        ),
        entry("/usr/bin/flatpak", host).replace(r#""why": "w""#, r#""why": "<b>x</b>""#),
    ] {
        assert!(parse(&file(&bad)).is_err(), "{bad}");
    }
    let twice = format!(
        "{}, {}",
        entry("/usr/bin/a", host),
        entry("/usr/bin/b", host)
    );
    assert!(parse(&file(&twice)).is_err(), "duplicate ids");
    assert!(parse(r#"{"version": 2, "entries": []}"#).is_err());
}

#[test]
fn a_requested_toggle_is_only_a_change_of_enabled() {
    let current = flatpak().rule();
    let mut wire = crate::rule_wire::rule_to_wire(&current);
    wire["enabled"] = false.into();
    assert_eq!(requested_toggle(&current, &wire), Some(false));
    let mut renamed = wire.clone();
    renamed["name"] = "my-flatpak".into();
    assert_eq!(requested_toggle(&current, &renamed), None);
    let mut wider = wire.clone();
    wider["operator"]["operands"][1]["data"] = "example.org".into();
    assert_eq!(requested_toggle(&current, &wider), None);
    assert_eq!(requested_toggle(&current, &serde_json::json!({})), None);
}

/// The curated layer's own path check, apart from the editor's.
#[test]
fn a_program_is_an_exact_path_under_usr() {
    assert!(usr_program("/usr/bin/flatpak"));
    for bad in [
        "/usr/bin/../bin/flatpak",
        "/usr/bin/./flatpak",
        "/usr//bin/flatpak",
        "/usr/",
        "/opt/flatpak",
        "/usr/local/bin/flatpak",
        "usr/bin/flatpak",
        "/usr/bin/flat\npak",
    ] {
        assert!(!usr_program(bad), "{bad:?}");
    }
}

/// The curated layer's own port check, apart from the editor's.
#[test]
fn a_port_is_ascii_digits_only() {
    let mut leaves = flatpak().rule().operator.unwrap().list;
    assert!(check_leaves(&leaves).is_ok());
    for bad in ["+443", "0443", "443 ", "65536", "0"] {
        leaves[2].data = bad.into();
        assert!(check_leaves(&leaves).is_err(), "{bad:?}");
    }
}
