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
//! `[A-Za-z0-9._-]`; this guards every name that didn't come from there. The
//! rules match the name validation in bazzite-tower's opensnitchd patch.

/// Longest accepted name, in bytes. `name + ".json"` must fit a 255-byte
/// filename, and the bridge's own generated names reach about 140 bytes.
pub const MAX_RULE_NAME_BYTES: usize = 200;

/// Name prefix of the rules Snitchwatch installs for blocklist subscriptions
/// (`z00-blocklist:<list>:<kind>`, issue #45). Reserved: a GUI can't add,
/// change or delete a rule under it, so it can't replace a blocklist's deny
/// with an allow of the same name. Those rules are managed on the
/// Blocklists page.
pub const BLOCKLIST_RULE_NAME_PREFIX: &str = "z00-blocklist:";

/// The band earlier builds used for the same rules. Reserved too: the
/// bridge's reconcile purges rules under it, so a GUI must not be able to
/// create one.
pub const LEGACY_BLOCKLIST_RULE_NAME_PREFIX: &str = "900-blocklist:";

/// Name prefix of the opensnitchd rules Snitchwatch ships in its packaging
/// (`packaging/bluebuild/files/system/etc/opensnitchd/rules/`). Reserved: a
/// GUI action or an import can't add, change or delete a rule under it.
/// Otherwise it could swap the packaged allow for a deny (no more list
/// downloads), or give it a temporary duration, after which the daemon
/// deletes its file.
pub const PACKAGED_RULE_NAME_PREFIX: &str = "000-snitchwatch-";

/// The packaged rule that lets the system bridge download blocklists
/// (`docs/superpowers/plans/2026-10-08-packaged-bridge-fetch-rule.md`).
pub const PACKAGED_FETCH_RULE_NAME: &str = "000-snitchwatch-bridge-fetch";

/// Name prefix reserved for the curated default rules Snitchwatch will
/// install and reconcile itself (prompt-slot plan, part D). Rules a GUI or a
/// file supplies may not use it, so they can't pose as, or be reconciled
/// away as, one of those.
pub const CURATED_DEFAULT_RULE_NAME_PREFIX: &str = "snitchwatch-default-";

/// Name prefix of the rules Snitchwatch installs for the active profile
/// (`850-profile:<profile>:<rule>`, issue #46 Part 2). Reserved: a GUI
/// can't add, change or delete a rule under it, so it can't replace a
/// profile's deny with an allow. Those rules are managed on the Profiles
/// page.
pub const PROFILE_RULE_NAME_PREFIX: &str = "850-profile:";

/// Whether `name` is under the profile prefix only the bridge may use.
pub fn is_reserved_profile_name(name: &str) -> bool {
    name.starts_with(PROFILE_RULE_NAME_PREFIX)
}

/// Whether `name` is under a blocklist prefix only the bridge may use.
pub fn is_reserved_blocklist_name(name: &str) -> bool {
    name.starts_with(BLOCKLIST_RULE_NAME_PREFIX)
        || name.starts_with(LEGACY_BLOCKLIST_RULE_NAME_PREFIX)
}

/// Whether `name` is under the prefix of the rules Snitchwatch ships.
pub fn is_reserved_packaged_name(name: &str) -> bool {
    name.starts_with(PACKAGED_RULE_NAME_PREFIX)
}

/// Whether `name` is under the prefix of the curated defaults.
pub fn is_reserved_curated_name(name: &str) -> bool {
    name.starts_with(CURATED_DEFAULT_RULE_NAME_PREFIX)
}

/// Whether `name` is under any prefix a GUI may not add, change or delete
/// (blocklists, profiles, packaged rules, curated defaults).
/// `DaemonCommands::send` refuses every command for such a name.
pub fn is_reserved_name(name: &str) -> bool {
    is_reserved_blocklist_name(name)
        || is_reserved_profile_name(name)
        || is_reserved_packaged_name(name)
        || is_reserved_curated_name(name)
}

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
    if name == "." || name == ".." {
        return Err("rule name is a relative path component".to_string());
    }
    if name.chars().any(char::is_control) {
        return Err("rule name contains a control character".to_string());
    }
    if name.chars().any(is_format_or_separator) {
        return Err("rule name contains an invisible format or separator character".to_string());
    }
    Ok(())
}

/// Unicode general categories Cf (format: bidi overrides, zero-width, soft
/// hyphen, tag characters, …), Zl and Zp — matching what bazzite-tower's
/// patched opensnitchd rejects, so a name the bridge forwards is never one
/// the daemon refuses. Listed explicitly (Unicode 15.1) to avoid a
/// dependency; an extra code point here only makes the bridge stricter.
fn is_format_or_separator(c: char) -> bool {
    matches!(c,
        '\u{00AD}'
        | '\u{0600}'..='\u{0605}'
        | '\u{061C}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08E2}'
        | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{2028}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{110BD}'
        | '\u{110CD}'
        | '\u{13430}'..='\u{1343F}'
        | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}'
        | '\u{E0020}'..='\u{E007F}'
    )
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
    fn rejects_what_the_patched_daemon_rejects() {
        // bazzite-tower's opensnitchd patch also validates names (empty, ".",
        // "..", separators, control, Unicode Cf/Zl/Zp, >200 bytes); the
        // bridge must reject at least the same set so a GUI action never
        // reaches a daemon that will refuse it.
        for name in [
            ".",
            "..",
            "soft\u{AD}hyphen",
            "rlo\u{202E}gpj.exe",
            "zw\u{200B}sp",
            "ls\u{2028}ps",
            "pp\u{2029}x",
            "alm\u{61C}x",
            "tag\u{E0041}x",
            "bom\u{FEFF}x",
        ] {
            assert!(validate_rule_name(name).is_err(), "accepted {name:?}");
        }
        assert!(
            validate_rule_name("...").is_ok(),
            "a single component of dots is a plain file"
        );
    }

    #[test]
    fn the_packaged_prefix_is_reserved_and_the_bridges_own_names_are_not() {
        assert!(is_reserved_packaged_name(PACKAGED_FETCH_RULE_NAME));
        assert!(is_reserved_name(PACKAGED_FETCH_RULE_NAME));
        assert!(is_reserved_name("000-snitchwatch-anything"));
        assert!(is_reserved_name("z00-blocklist:ads:domains"));
        assert!(!is_reserved_blocklist_name(PACKAGED_FETCH_RULE_NAME));
        assert!(validate_rule_name(PACKAGED_FETCH_RULE_NAME).is_ok());
        for name in [
            "000-allow-localhost",
            "000-snitchwatch",
            "000-Snitchwatch-bridge-fetch",
            rule_name_for(Verdict::Allow, "github.com", 443, "/usr/bin/curl").as_str(),
            rule_name_for(Verdict::Deny, "example.com", 80, "").as_str(),
        ] {
            assert!(!is_reserved_name(name), "{name}");
        }
    }

    /// Issue #46 Part 2: profile rules are the bridge's own, like blocklist
    /// rules.
    #[test]
    fn the_profile_prefix_is_reserved() {
        assert!(is_reserved_profile_name("850-profile:home:0000-r1"));
        assert!(is_reserved_name("850-profile:home:0000-r1"));
        assert!(is_reserved_name(PROFILE_RULE_NAME_PREFIX));
        for name in ["850-profile", "850-Profile:home", "851-profile:home"] {
            assert!(!is_reserved_name(name), "{name}");
        }
    }

    #[test]
    fn errors_do_not_echo_the_name() {
        let err = validate_rule_name("../<b>evil</b>").unwrap_err();
        assert!(!err.contains("evil"), "{err}");
    }
}
