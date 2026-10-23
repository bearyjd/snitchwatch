//! The versioned-binary directory rule (the `2.1.308` case).
//!
//! When the 0a heuristic reads a program's path as
//! `<dir>/<app>/versions/<semver>`, the review OFFERS one rule for the whole
//! folder, because the program's path changes with every update. The
//! heuristic is a suggestion only. The rule is a `process.path` regular
//! expression, matched case-sensitively and anchored:
//!
//! `^<dir>/<app>/versions/[^/]*[^/)]$`
//!
//! * `<dir>` and `<app>` are escaped, so a dot or a bracket in a folder name
//!   is a dot or a bracket.
//! * `[^/]*` stays inside the folder: a path with a further `/` after the
//!   version segment is not matched.
//! * The last character may not be `)`, so a deleted file
//!   (`.../2.1.308 (deleted)`) is not matched. This is stricter than a bare
//!   `[^/]+` on purpose: `[^/]+` would also match the deleted file.
//!
//! The rule is shown to the owner in full ([`DirectoryRule::text`]), so none
//! is offered when the folder holds a character that would be hidden or
//! changed on the way (bidi controls, zero-width characters, tabs).
//!
//! Checked in the tests with the GUI's RE2 simulator and the bridge's rule
//! policy (`check_wire_rule`, both the editor and import profiles; its first
//! step is the `rule_from_wire` an `AddRule` goes through).
//!
//! NOT checked here: a live opensnitchd compiling the pattern (the simulator
//! reproduces Go's `regexp` only for the class forms it models), and the
//! bridge's wording for such a rule. The bridge's `binds_to_programs` reads
//! only an exact `process.path`, so its import preview and rule editor call
//! this rule "all apps" (the known limit in the plan).

use super::paths::selectability;
use super::types::DirectoryRule;
use crate::app_names::{is_stripped, versioned_binary};
use crate::learned::{path_flag, Program};

/// Whether the folder shows exactly as it is: no character `sanitize_display`
/// would drop or turn into a space.
fn shows_in_full(folder: &str) -> bool {
    !folder
        .chars()
        .any(|c| is_stripped(c) || (c.is_whitespace() && c != ' '))
}

/// The directory rule for `program`, when a rule can be made for it at all
/// and its path has the versions shape.
pub fn directory_rule(program: &Program) -> Option<DirectoryRule> {
    let path = selectability(program).ok()?;
    let binary = versioned_binary(path)?;
    let folder = format!("{}/{}/versions/", binary.dir, binary.app);
    if !shows_in_full(&folder) {
        return None;
    }
    let pattern = format!(
        "^{}/{}/versions/[^/]*[^/)]$",
        regex::escape(binary.dir),
        regex::escape(binary.app)
    );
    Some(DirectoryRule {
        user_writable: path_flag(&folder).is_some(),
        folder,
        pattern,
        sensitive: true,
    })
}
