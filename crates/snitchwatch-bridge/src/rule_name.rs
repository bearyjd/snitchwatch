//! Validation for rule names that reach opensnitchd from outside this
//! bridge's own name generator (a GUI's `AddRule`/`UpdateRule`/`DeleteRule`).
//!
//! opensnitchd runs as root and writes a persisted rule to
//! `filepath.Join(rulesDir, name + ".json")` (`daemon/rule/loader.go` `Add`/
//! `Replace` → `Save`), and deletes it via plain string concatenation
//! (`deleteRuleFromDisk`). It does not validate the name itself, so a name
//! containing a path separator — e.g. `../default-config` — makes root write
//! (or later delete) a file outside the rules directory. Names the bridge
//! generates (`translator::verdict::rule_name_for`) are already restricted to
//! `[A-Za-z0-9._-]`; this guards every name that didn't come from there.

/// Longest accepted name, in bytes. `name + ".json"` must fit a 255-byte
/// filename, and the bridge's own generated names reach about 140 bytes.
pub const MAX_RULE_NAME_BYTES: usize = 200;

/// Reject a rule name that could escape the daemon's rules directory or
/// isn't a sane single filename. The error never echoes the name: it is
/// attacker-influenced and error strings reach logs and the GUI.
pub fn validate_rule_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("rule name is empty".to_string());
    }
    if name.len() > MAX_RULE_NAME_BYTES {
        return Err(format!(
            "rule name is {} bytes; the limit is {MAX_RULE_NAME_BYTES}",
            name.len()
        ));
    }
    // A separator is the only way out of the rules directory: the daemon
    // always appends ".json", so a single component can never be `..`.
    if name.contains('/') || name.contains('\\') {
        return Err("rule name contains a path separator".to_string());
    }
    if name.chars().any(char::is_control) {
        return Err("rule name contains a control character".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::connections::Verdict;
    use crate::translator::verdict::rule_name_for;

    #[test]
    fn accepts_names_the_bridge_generates() {
        let long_host = format!("{}.example.com", "a".repeat(60));
        let long_path = format!("/opt/{}/bin/{}", "d".repeat(80), "b".repeat(60));
        for name in [
            rule_name_for(Verdict::Allow, "github.com", 443, ""),
            rule_name_for(Verdict::Deny, &long_host, 65535, &long_path),
            rule_name_for(
                Verdict::Allow,
                "../../../../etc/cron.d/x",
                443,
                "/tmp/../x y",
            ),
        ] {
            assert!(validate_rule_name(&name).is_ok(), "rejected {name}");
        }
    }

    #[test]
    fn accepts_ordinary_names_from_other_clients() {
        for name in [
            "allow-always-curl",
            "000-allow-localhost",
            "deny firefox",
            "a..b",
        ] {
            assert!(validate_rule_name(name).is_ok(), "rejected {name}");
        }
    }

    #[test]
    fn rejects_names_that_could_leave_the_rules_directory() {
        for name in [
            "../default-config",
            "../../../../etc/cron.d/x",
            "sub/dir",
            "/etc/passwd",
            r"..\windows",
        ] {
            assert!(validate_rule_name(name).is_err(), "accepted {name}");
        }
    }

    #[test]
    fn rejects_empty_control_and_oversized_names() {
        let oversized = "a".repeat(MAX_RULE_NAME_BYTES + 1);
        for name in [
            "",
            "nul\0byte",
            "new\nline",
            "tab\tname",
            "del\u{7f}",
            oversized.as_str(),
        ] {
            assert!(validate_rule_name(name).is_err(), "accepted {name:?}");
        }
        assert!(validate_rule_name(&"a".repeat(MAX_RULE_NAME_BYTES)).is_ok());
    }

    #[test]
    fn errors_do_not_echo_the_name() {
        let err = validate_rule_name("../<b>evil</b>").unwrap_err();
        assert!(!err.contains("evil"), "{err}");
    }
}
