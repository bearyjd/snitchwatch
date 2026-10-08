//! Shape policy for rule operators that reach root opensnitchd from a GUI
//! (`AddRule`/`UpdateRule` → [`crate::rule_wire::rule_from_wire`] →
//! `CHANGE_RULE`).
//!
//! opensnitchd v1.8.0 (`vendor/opensnitch/daemon/rule/operator.go`) picks
//! *what* to compare by the operand (`Match`: `dest.network`/`source.network`
//! pass a `net.IP`, the rest pass strings) but *how* by the type (`Compile`),
//! and the type's callback does hard type assertions with no `recover()`:
//!
//! - `simple` + `dest.network` reaches `simpleCmp`'s `v.(string)` and
//!   `network` + `dest.ip` reaches `cmpNetwork`'s `value.(net.IP)`: the
//!   daemon panics on the first matching connection. An `always` rule is
//!   saved and reloaded, so it crash-loops, and with `QueueBypass` the
//!   firewall fails open while the daemon is down.
//! - `Match` checks the operand `list` before the type, and `listMatch`
//!   starts from `true`: a list with no members, or a non-list type with
//!   the `list` operand, matches every connection. `Deserialize`
//!   (`rule.go`) copies list members only one level deep and drops members
//!   of a non-list type, so a nested list reaches the daemon empty.
//! - `lists.*` operands make the legacy daemon read every file under the
//!   directory named in `data`, as root. Snitchwatch creates those rules
//!   itself (blocklists), never from a GUI's rule; bazzite-tower's patched
//!   daemon refuses them from a UI too.
//! - `process.hash.*` matches every connection while the daemon computes no
//!   checksums (its default), so a hash leaf, or a list no other member
//!   narrows, is refused. So is an empty regexp (a missing `data` arrives
//!   as `""`), and a pattern Go's RE2 would read differently once the
//!   daemon lowercases it (see `regexp`).
//! - `Compile` overwrites a `simple` `user.name` leaf's data with the uid,
//!   and the daemon reports the rule that way. Sent back, the uid is looked
//!   up as a *name*: an enabled rule is refused, and a disabled one is
//!   saved and then never compiles again. So a numeric `user.name` is
//!   refused, and such a rule is listed read-only.
//!
//! [`validate_operator`] accepts only shapes that evaluate as written: a leaf
//! (`simple`, `regexp`, `network`) or one `list` of 1..=64 leaves. Every
//! shape the bridge itself builds (`translator::verdict`) passes. It runs
//! whatever the rule's `enabled` flag says: the daemon compiles only enabled
//! rules, so a disabled bad shape is stored unchecked until something
//! enables it.
//!
//! Errors are fixed text and never echo the operand or data: both are
//! client-supplied, and the errors reach logs.

use snitchwatch_proto::protocol::{Operator, Rule};

use crate::cache::rules::{MAX_OPERATOR_LIST_LEN, MAX_RULE_FIELD_BYTES};

mod narrowing;
mod profile;
mod regexp;

pub use narrowing::binds_to_programs;

pub use profile::{validate_user_rule, PolicyProfile, RuleProblem};

/// Parse a rule from the wire shape and check it for `profile`: the one
/// path the bridge and the GUIs use for a rule written or imported whole.
/// A wire error is reported as a problem at `rule`.
pub fn check_wire_rule(
    value: &serde_json::Value,
    profile: PolicyProfile,
) -> Result<Rule, Vec<RuleProblem>> {
    let rule = crate::rule_wire::rule_from_wire(value).map_err(|reason| {
        vec![RuleProblem {
            path: "rule".into(),
            reason,
        }]
    })?;
    validate_user_rule(&rule, profile)?;
    Ok(rule)
}

/// Why a GUI may not change a daemon rule whose operator fails
/// [`validate_operator`] (a `lists` blocklist rule, a network alias such as
/// `LAN`, or a shape the daemon can't evaluate). The rule is still listed,
/// the bridge refuses to send it back in a change, and it stays
/// [`deletable`]: a delete names the rule and nothing else.
pub const SHAPE_READ_ONLY_REASON: &str = "Snitchwatch can't change this rule because of its \
     conditions (a rule type, condition or network alias Snitchwatch won't send back to the \
     firewall service). The rule still applies; you can still delete it.";

/// Why a GUI may not change or delete a rule under a blocklist name
/// ([`crate::rule_name::is_reserved_blocklist_name`]): Snitchwatch installs
/// and removes those itself, from the Blocklists page (issue #45).
pub const BLOCKLIST_MANAGED_REASON: &str =
    "Managed on the Blocklists page. Subscribe to or remove the list there.";

/// Why a GUI may not change or delete the packaged fetch rule
/// ([`crate::rule_name::PACKAGED_FETCH_RULE_NAME`]).
pub const PACKAGED_FETCH_RULE_REASON: &str =
    "Built into Snitchwatch: lets its background service download blocklists.";

/// Why a GUI may not change or delete any other rule under the packaged
/// prefix ([`crate::rule_name::is_reserved_packaged_name`]).
pub const PACKAGED_RULE_REASON: &str =
    "Uses a name reserved for rules built into Snitchwatch, so Snitchwatch doesn't change or \
     delete it.";

/// Why a GUI may not change or delete a rule under the curated-defaults
/// prefix ([`crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX`]). No such
/// rules exist before prompt-slot D, so this says what is true today: the
/// name is reserved.
pub const CURATED_MANAGED_REASON: &str = "This name is reserved for Snitchwatch's own rules, so \
     Snitchwatch won't change or delete it. The rule still applies.";

/// Operands whose value the daemon passes as a `net.IP`; only the `network`
/// type can compare one.
const NETWORK_OPERANDS: &[&str] = &["dest.network", "source.network"];

/// Operands whose value the daemon passes as a string (`operator.go`'s
/// `Operand` consts, minus `true`, `list`, the network ones and `lists.*`).
/// `process.env.<NAME>` is a prefix and is checked separately.
const STRING_OPERANDS: &[&str] = &[
    "process.id",
    "process.path",
    "process.parent.path",
    "process.command",
    "process.hash.md5",
    "process.hash.sha1",
    "user.id",
    "user.name",
    "source.ip",
    "source.port",
    "dest.ip",
    "dest.host",
    "dest.port",
    "protocol",
    "iface.in",
    "iface.out",
];

const ENV_OPERAND_PREFIX: &str = "process.env.";

/// `Match` returns true for these before any comparison when checksums are
/// off (opensnitchd's default), and `hashCmp` returns true for an empty
/// hash: on its own, such a leaf matches every connection.
const HASH_OPERANDS: &[&str] = &["process.hash.md5", "process.hash.sha1"];

const HASH_MATCHES_ALL: &str = "a process hash condition matches every connection while the \
     firewall service doesn't compute checksums; combine it with another condition";

/// What `Match` hands a leaf's callback for its operand.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OperandKind {
    /// `true`: matches without calling the callback.
    True,
    /// A `net.IP`.
    Network,
    /// A string.
    Text,
}

/// Check that an operator from a GUI has a shape opensnitchd evaluates as
/// written: no panic, no silent match-all, no blocklist directory read.
pub fn validate_operator(op: &Operator) -> Result<(), String> {
    if op.r#type == "list" {
        return validate_list(op);
    }
    validate_leaf(op)?;
    if HASH_OPERANDS.contains(&op.operand.as_str()) {
        return Err(HASH_MATCHES_ALL.to_string());
    }
    Ok(())
}

/// Why a daemon rule is listed read-only, or `None` when a GUI may edit it.
/// Uses the same checks as [`crate::rule_wire::rule_from_wire`], so a rule
/// the GUI may edit is one the bridge will send back.
pub fn read_only_reason(rule: &Rule) -> Option<&'static str> {
    if crate::rule_name::is_reserved_blocklist_name(&rule.name) {
        return Some(BLOCKLIST_MANAGED_REASON);
    }
    if rule.name == crate::rule_name::PACKAGED_FETCH_RULE_NAME {
        return Some(PACKAGED_FETCH_RULE_REASON);
    }
    if crate::rule_name::is_reserved_packaged_name(&rule.name) {
        return Some(PACKAGED_RULE_REASON);
    }
    if crate::rule_name::is_reserved_curated_name(&rule.name) {
        return Some(CURATED_MANAGED_REASON);
    }
    if crate::rule_name::validate_rule_name(&rule.name).is_err() {
        return Some(crate::rule_wire::READ_ONLY_REASON);
    }
    match &rule.operator {
        Some(op) if validate_operator(op).is_ok() => None,
        _ => Some(SHAPE_READ_ONLY_REASON),
    }
}

/// Whether a GUI may delete a daemon rule. `DELETE_RULE` carries only the
/// name (`Loader.Delete` never reads the operator), so this is the name
/// check `notification_for_effect` applies to a `DeleteRule`: a rule
/// read-only only for its conditions stays deletable. A blocklist rule is
/// removed from the Blocklists page instead, and a packaged rule not at all.
pub fn deletable(rule: &Rule) -> bool {
    crate::rule_name::validate_rule_name(&rule.name).is_ok()
        && !crate::rule_name::is_reserved_name(&rule.name)
}

fn validate_list(op: &Operator) -> Result<(), String> {
    // `Compile` sets a list's operand to `list`; `operator_from_wire` leaves
    // it empty.
    if !(op.operand.is_empty() || op.operand == "list") {
        return Err("a list operator's operand must be \"list\"".to_string());
    }
    if op.list.is_empty() {
        return Err("a list operator has no members; it would match every connection".to_string());
    }
    if op.list.len() > MAX_OPERATOR_LIST_LEN {
        return Err(format!(
            "a list operator has {} members; the limit is {MAX_OPERATOR_LIST_LEN}",
            op.list.len()
        ));
    }
    for (index, member) in op.list.iter().enumerate() {
        validate_leaf(member).map_err(|e| format!("list member {}: {e}", index + 1))?;
    }
    // Members are ANDed; `true` and hash members never narrow the list
    // while checksums are off. An all-`true` list is as explicit as `true`.
    let has_hash = op
        .list
        .iter()
        .any(|m| HASH_OPERANDS.contains(&m.operand.as_str()));
    let constrained = op
        .list
        .iter()
        .any(|m| m.operand != "true" && !HASH_OPERANDS.contains(&m.operand.as_str()));
    if has_hash && !constrained {
        return Err(HASH_MATCHES_ALL.to_string());
    }
    Ok(())
}

fn validate_leaf(op: &Operator) -> Result<(), String> {
    if !op.list.is_empty() {
        return Err("only a list operator may have members".to_string());
    }
    if op.operand.len() > MAX_RULE_FIELD_BYTES || op.data.len() > MAX_RULE_FIELD_BYTES {
        return Err(format!(
            "operator operand or data is over {MAX_RULE_FIELD_BYTES} bytes"
        ));
    }
    match op.r#type.as_str() {
        "simple" if op.operand == "user.name" && is_uid(&op.data) => {
            Err(USER_NAME_IS_A_UID.to_string())
        }
        "simple" => match operand_kind(&op.operand)? {
            OperandKind::True | OperandKind::Text => Ok(()),
            OperandKind::Network => Err(NETWORK_OPERAND_NEEDS_NETWORK.to_string()),
        },
        // `Compile` maps a user name to its uid only for `simple`; a regexp
        // is compared with the uid string and never matches a name.
        "regexp" if op.operand == "user.name" => {
            Err("the user.name operand needs the simple type".to_string())
        }
        "regexp" => match operand_kind(&op.operand)? {
            OperandKind::Text => regexp::validate_regexp(&op.data, op.sensitive),
            OperandKind::True => Err(TRUE_NEEDS_SIMPLE.to_string()),
            OperandKind::Network => Err(NETWORK_OPERAND_NEEDS_NETWORK.to_string()),
        },
        "network" => match operand_kind(&op.operand)? {
            OperandKind::Network => validate_cidr(&op.data),
            OperandKind::True | OperandKind::Text => {
                Err("the network type needs the dest.network or source.network operand".to_string())
            }
        },
        "list" => Err(
            "a list can't contain a list; the daemon drops the inner members and the empty \
             list matches every connection"
                .to_string(),
        ),
        "lists" => Err(LISTS_REFUSED.to_string()),
        _ => Err("operator type is not simple, regexp, network or list".to_string()),
    }
}

const NETWORK_OPERAND_NEEDS_NETWORK: &str =
    "the dest.network and source.network operands need the network type";
const TRUE_NEEDS_SIMPLE: &str = "the true operand needs the simple type";
const USER_NAME_IS_A_UID: &str = "this user.name condition holds the uid the firewall service \
     reports once the rule is loaded; sent back, it would be looked up as a user name and the \
     rule would stop loading";
const LISTS_REFUSED: &str =
    "blocklist (lists) rules are managed by Snitchwatch and can't be sent from a GUI";

fn operand_kind(operand: &str) -> Result<OperandKind, String> {
    if operand == "true" {
        return Ok(OperandKind::True);
    }
    if NETWORK_OPERANDS.contains(&operand) {
        return Ok(OperandKind::Network);
    }
    if STRING_OPERANDS.contains(&operand) {
        return Ok(OperandKind::Text);
    }
    if operand
        .strip_prefix(ENV_OPERAND_PREFIX)
        .is_some_and(is_env_var_name)
    {
        return Ok(OperandKind::Text);
    }
    if operand == "list" {
        return Err("the list operand needs the list type".to_string());
    }
    if operand.starts_with("lists.") {
        return Err(LISTS_REFUSED.to_string());
    }
    Err("unknown operator operand".to_string())
}

/// What `Compile` writes over a `user.name` leaf's data: a decimal uid.
/// `useradd` refuses fully numeric names, so no real name looks like this.
fn is_uid(data: &str) -> bool {
    !data.is_empty() && data.bytes().all(|b| b.is_ascii_digit())
}

/// A portable environment variable name (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `network` data must be a CIDR (`net.ParseCIDR`: address, `/`, decimal
/// prefix length). The daemon would also accept a network alias such as
/// `LAN`, but aliases come from its own `network_aliases.json`, which the
/// bridge can't see. An unknown alias fails `Compile`: for an enabled rule
/// `replaceUserRule` returns that error and the daemon answers the
/// `CHANGE_RULE` with ERROR, but a disabled rule is saved without compiling
/// and only fails once it is enabled.
fn validate_cidr(data: &str) -> Result<(), String> {
    const NOT_A_CIDR: &str = "network operator data is not a CIDR such as 10.0.0.0/8";
    // Go's `IPNet.Contains` reads a network on an IPv4-mapped address as
    // IPv4 with the mask's last 32 bits: `::ffff:0:0/96` matches every IPv4
    // address. Only the IPv4 form says what it means.
    const MAPPED: &str = "network operator data is an IPv4 network written as IPv6 \
         (::ffff:…); write it as IPv4, such as 10.0.0.0/8";
    let (addr, prefix) = data.split_once('/').ok_or(NOT_A_CIDR)?;
    let max_prefix = match addr.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) => 32,
        Ok(std::net::IpAddr::V6(v6)) if v6.to_ipv4_mapped().is_some() => {
            return Err(MAPPED.to_string())
        }
        Ok(std::net::IpAddr::V6(_)) => 128,
        Err(_) => return Err(NOT_A_CIDR.to_string()),
    };
    let digits_only =
        !prefix.is_empty() && prefix.len() <= 3 && prefix.bytes().all(|b| b.is_ascii_digit());
    match prefix.parse::<u16>() {
        Ok(len) if digits_only && len <= max_prefix => Ok(()),
        _ => Err(NOT_A_CIDR.to_string()),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod profile_tests;

#[cfg(test)]
mod schema_tests;

#[cfg(test)]
mod editor_tests;
