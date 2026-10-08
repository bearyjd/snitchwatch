//! Profile layers on top of [`super::validate_operator`] for rules that come
//! from outside the bridge as whole rules, not as edits of a daemon rule.
//!
//! [`validate_user_rule`] runs `validate_operator` first (pairing, list
//! shape, `lists.*`, regexps, CIDRs, hash-only shapes) and, only for a shape
//! it accepts, the profile's own checks. The `Import` profile (rule
//! import, roadmap P2.7) treats every rule as untrusted file content.
//!
//! Every reason is fixed text: neither a reason nor a path ever echoes the
//! rule's name, operand or data (the `rule_name.rs` pattern). Paths are built
//! from field names and list indices only, e.g. `operator.list[1].data`.

use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::{Operator, Rule};

/// Which caller a rule comes from; each adds its own checks on top of
/// `validate_operator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyProfile {
    /// A rule read from an import file.
    Import,
    /// A rule written or changed in the rule editor (roadmap P2.1).
    Editor,
    /// A profile's rule as the bridge installs it (issue #46 Part 2).
    ProfileRule,
}

/// One reason a rule is refused. `reason` is always fixed text (or
/// `validate_operator`'s, which is fixed text plus counts and indices).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleProblem {
    pub path: String,
    pub reason: String,
}

pub const NO_CONDITIONS: &str = "the rule has no conditions";
pub const BLOCKLIST_NAME_REFUSED: &str = "names starting with z00-blocklist: or 900-blocklist: \
     belong to Snitchwatch's blocklist rules";
pub const CURATED_NAME_REFUSED: &str =
    "names starting with snitchwatch-default- belong to Snitchwatch's own default rules";
/// Rules Snitchwatch ships ready-made (`rule_name::PACKAGED_RULE_NAME_PREFIX`).
pub const PACKAGED_NAME_REFUSED: &str =
    "names starting with 000-snitchwatch- belong to rules Snitchwatch ships";
pub const ACTION_REFUSED: &str = "the action must be allow, deny or reject";
pub const DURATION_REFUSED: &str = "only rules that last always or until the firewall restarts \
     can be imported; once and timed rules can't";
pub const TOO_LARGE: &str = "a field is over 16 KiB, or the conditions nest too deeply";
pub const HASH_REFUSED: &str = "process hash conditions can't be imported: the firewall service \
     treats them as matching every program while it doesn't compute checksums";
pub const MATCHES_EVERYTHING: &str = "this rule matches every connection: none of its \
     conditions narrows it (\"true\", a /0 network, or a pattern that matches everything); it \
     can't be imported";
pub const EMPTY_VALUE_REFUSED: &str =
    "an empty value matches far more than it looks; only a host name may be empty";
pub const EDITOR_DURATION_REFUSED: &str = "the duration must be always, until the firewall \
     restarts, or a time from 10s to 365 days written like 30s, 5m or 1h30m";
pub const RELATIVE_PATH_REFUSED: &str = "an exact program path must be the program's full path, \
     starting with /; use a pattern to match more than one program";
pub const PROTOCOL_REFUSED: &str = "a protocol is a short lowercase name such as tcp or udp";
pub const EDITOR_EMPTY_HOST_REFUSED: &str = "a blank host name matches every connection that \
     has no host name, such as every connection to a bare address; name a host, or use an IP \
     address or network condition";
pub const EDITOR_OPERAND_REFUSED: &str = "process ID and environment conditions can't be \
     written here: a process ID names whatever process gets that number next, and a program \
     sets its own environment";
pub const PORT_REFUSED: &str = "a port must be a whole number from 0 to 65535";
pub const ID_REFUSED: &str = "a process or user ID must be a whole number";
pub const PROFILE_NAME_REFUSED: &str =
    "names starting with 850-profile: belong to Snitchwatch's profile rules";
pub const PROFILE_PREFIX_REQUIRED: &str = "a profile rule's name must start with 850-profile:";
pub const PROFILE_DURATION_REFUSED: &str =
    "a profile rule lasts while its profile is active, so it can't have a time of its own";
pub const PROFILE_PRECEDENCE_REFUSED: &str = "a profile rule can't decide before other rules: \
     blocking rules and blocklists win over a profile";
pub const PROFILE_NOLOG_REFUSED: &str = "a profile rule can't hide its connections";
pub const PROFILE_CASE_REFUSED: &str =
    "an exact program path in a profile rule must match upper and lower case exactly";
pub const PROFILE_USER_NAME_REFUSED: &str = "a profile rule can't match a user by name: the \
     firewall stores it as a number, so Snitchwatch couldn't tell the rule is in place; use the \
     user ID";

/// Check a whole rule for `profile`. Returns every problem found.
pub fn validate_user_rule(rule: &Rule, profile: PolicyProfile) -> Result<(), Vec<RuleProblem>> {
    let mut problems = Vec::new();
    match profile {
        PolicyProfile::Import | PolicyProfile::Editor | PolicyProfile::ProfileRule => {
            check_rule(rule, profile, &mut problems)
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn problem(problems: &mut Vec<RuleProblem>, path: &str, reason: impl Into<String>) {
    problems.push(RuleProblem {
        path: path.to_string(),
        reason: reason.into(),
    });
}

/// The checks every profile shares; they differ only in which durations
/// they take and in the editor's extra checks on what is typed by hand.
fn check_rule(rule: &Rule, profile: PolicyProfile, problems: &mut Vec<RuleProblem>) {
    match &rule.operator {
        None => problem(problems, "operator", NO_CONDITIONS),
        Some(op) => match super::validate_operator(op) {
            Err(reason) => problem(problems, "operator", reason),
            Ok(()) => check_import_operator(op, profile, problems),
        },
    }
    if let Err(reason) = crate::rule_name::validate_rule_name(&rule.name) {
        problem(problems, "name", reason);
    }
    if crate::rule_name::is_reserved_blocklist_name(&rule.name) {
        problem(problems, "name", BLOCKLIST_NAME_REFUSED);
    }
    if rule
        .name
        .starts_with(crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX)
    {
        problem(problems, "name", CURATED_NAME_REFUSED);
    }
    if crate::rule_name::is_reserved_packaged_name(&rule.name) {
        problem(problems, "name", PACKAGED_NAME_REFUSED);
    }
    let profile_name = crate::rule_name::is_reserved_profile_name(&rule.name);
    match profile {
        PolicyProfile::ProfileRule if !profile_name => {
            problem(problems, "name", PROFILE_PREFIX_REQUIRED)
        }
        PolicyProfile::Import | PolicyProfile::Editor if profile_name => {
            problem(problems, "name", PROFILE_NAME_REFUSED)
        }
        _ => {}
    }
    if !matches!(rule.action.as_str(), "allow" | "deny" | "reject") {
        problem(problems, "action", ACTION_REFUSED);
    }
    match profile {
        PolicyProfile::Import if !matches!(rule.duration.as_str(), "always" | "until restart") => {
            problem(problems, "duration", DURATION_REFUSED)
        }
        PolicyProfile::Editor if !editor_duration(&rule.duration) => {
            problem(problems, "duration", EDITOR_DURATION_REFUSED)
        }
        PolicyProfile::ProfileRule => check_profile_fields(rule, problems),
        _ => {}
    }
    if !crate::cache::rules::within_limits(rule) {
        problem(problems, "rule", TOO_LARGE);
    }
}

/// The import checks for an operator `validate_operator` accepted: a leaf,
/// or one list of leaves.
fn check_import_operator(op: &Operator, profile: PolicyProfile, problems: &mut Vec<RuleProblem>) {
    let leaves = leaves_of(op);
    // Members are ANDed: a list narrows as soon as one member does.
    if !leaves
        .iter()
        .any(|(_, leaf)| super::narrowing::narrows(leaf))
    {
        problem(problems, "operator", MATCHES_EVERYTHING);
    }
    for (path, leaf) in leaves {
        check_import_leaf(&path, leaf, problems);
        if profile != PolicyProfile::Import {
            check_editor_leaf(&path, leaf, problems);
        }
        if profile == PolicyProfile::ProfileRule {
            check_profile_leaf(&path, leaf, problems);
        }
    }
}

/// A profile rule lasts while its profile is active, and blocking rules
/// win over it (owner decision: precedence stays false).
fn check_profile_fields(rule: &Rule, problems: &mut Vec<RuleProblem>) {
    if rule.duration != "always" {
        problem(problems, "duration", PROFILE_DURATION_REFUSED);
    }
    if rule.precedence {
        problem(problems, "precedence", PROFILE_PRECEDENCE_REFUSED);
    }
    if rule.nolog {
        problem(problems, "nolog", PROFILE_NOLOG_REFUSED);
    }
}

/// Part 1's findings for profile rules (the editor's checks already refuse
/// an empty host): a case-folded exact path is #50's bug, and a
/// `user.name` comes back from the daemon as a number.
fn check_profile_leaf(path: &str, leaf: &Operator, problems: &mut Vec<RuleProblem>) {
    let simple = leaf.r#type == "simple";
    match leaf.operand.as_str() {
        "process.path" if simple && !leaf.sensitive => {
            problem(problems, &format!("{path}.data"), PROFILE_CASE_REFUSED)
        }
        "user.name" => problem(
            problems,
            &format!("{path}.operand"),
            PROFILE_USER_NAME_REFUSED,
        ),
        _ => {}
    }
}

/// Shortest and longest timed rule the editor writes: the daemon parses
/// the duration only after storing the rule (`replaceUserRule`), so one it
/// can't parse would never expire.
const MIN_TIMED_SECS: i64 = 10;
const MAX_TIMED_SECS: i64 = 365 * 24 * 3600;

fn editor_duration(duration: &str) -> bool {
    matches!(duration, "always" | "until restart")
        || crate::cache::rules::parse_duration_secs(duration)
            .is_some_and(|secs| (MIN_TIMED_SECS..=MAX_TIMED_SECS).contains(&secs))
}

/// What the editor adds for values typed by hand (owner decision E2): an
/// exact program path is a real program's full path (#44's rule for
/// remembered verdicts), and a protocol is a short lowercase token.
fn check_editor_leaf(path: &str, leaf: &Operator, problems: &mut Vec<RuleProblem>) {
    if leaf.operand == "process.id" || leaf.operand.starts_with("process.env.") {
        problem(problems, &format!("{path}.operand"), EDITOR_OPERAND_REFUSED);
    }
    if leaf.r#type != "simple" {
        return;
    }
    let data_path = format!("{path}.data");
    if leaf.data.is_empty() {
        if leaf.operand == "dest.host" {
            problem(problems, &data_path, EDITOR_EMPTY_HOST_REFUSED);
        }
        return;
    }
    match leaf.operand.as_str() {
        "process.path"
            if !crate::translator::process_binding::is_bindable_process_path(&leaf.data) =>
        {
            problem(problems, &data_path, RELATIVE_PATH_REFUSED)
        }
        "protocol"
            if !(leaf.data.len() <= 16
                && leaf
                    .data
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())) =>
        {
            problem(problems, &data_path, PROTOCOL_REFUSED)
        }
        _ => {}
    }
}

fn check_import_leaf(path: &str, leaf: &Operator, problems: &mut Vec<RuleProblem>) {
    if leaf.operand.starts_with("process.hash.") {
        problem(problems, &format!("{path}.operand"), HASH_REFUSED);
    }
    // Only `simple` compares the data as a value; a `regexp` is a pattern.
    if leaf.r#type != "simple" {
        return;
    }
    let data_path = format!("{path}.data");
    // `simpleCmp` is `EqualFold`: an empty value matches every subject that
    // lacks the field (an unset variable, say). An empty `dest.host` is the
    // one kept: it matches every connection without a host name (a bare IP
    // address), which is broad but what it says.
    if leaf.data.is_empty() && !matches!(leaf.operand.as_str(), "true" | "dest.host") {
        problem(problems, &data_path, EMPTY_VALUE_REFUSED);
        return;
    }
    match leaf.operand.as_str() {
        "source.port" | "dest.port" if !is_decimal::<u16>(&leaf.data) => {
            problem(problems, &data_path, PORT_REFUSED)
        }
        "process.id" | "user.id" if !is_decimal::<u32>(&leaf.data) => {
            problem(problems, &data_path, ID_REFUSED)
        }
        _ => {}
    }
}

/// What stops a rule from being turned on, whatever made it (re-review M2):
/// conditions that match every connection (`true`, a `/0` network, a
/// pattern that matches everything, or only a process hash, which matches
/// every program while checksums are off), an empty value, or a duration
/// the editor wouldn't write. Turning a rule off is never checked.
pub fn enable_problems(rule: &Rule) -> Vec<RuleProblem> {
    let mut problems = Vec::new();
    match &rule.operator {
        None => problem(&mut problems, "operator", NO_CONDITIONS),
        Some(op) => {
            let leaves = leaves_of(op);
            let narrows = |leaf: &Operator| {
                super::narrowing::narrows(leaf) && !leaf.operand.starts_with("process.hash.")
            };
            if !leaves.iter().any(|(_, leaf)| narrows(leaf)) {
                problem(&mut problems, "operator", MATCHES_EVERYTHING);
            }
            for (path, leaf) in leaves {
                let empty = leaf.r#type == "simple" && leaf.data.is_empty();
                let reason = match leaf.operand.as_str() {
                    "true" => continue,
                    "dest.host" => EDITOR_EMPTY_HOST_REFUSED,
                    _ => EMPTY_VALUE_REFUSED,
                };
                if empty {
                    problem(&mut problems, &format!("{path}.data"), reason);
                }
            }
        }
    }
    if !editor_duration(&rule.duration) {
        problem(&mut problems, "duration", EDITOR_DURATION_REFUSED);
    }
    problems
}

/// A leaf, or one list's members, each with its path.
fn leaves_of(op: &Operator) -> Vec<(String, &Operator)> {
    if op.r#type == "list" {
        op.list
            .iter()
            .enumerate()
            .map(|(index, member)| (format!("operator.list[{index}]"), member))
            .collect()
    } else {
        vec![("operator".to_string(), op)]
    }
}

/// Digits only (no sign, no `0x`), and within `T`'s range.
fn is_decimal<T: std::str::FromStr>(data: &str) -> bool {
    !data.is_empty() && data.bytes().all(|b| b.is_ascii_digit()) && data.parse::<T>().is_ok()
}
