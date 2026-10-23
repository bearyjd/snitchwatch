//! Building the rule payloads a later phase would send. Nothing here sends.
//!
//! The payloads come from the bridge's own builder (`verdict_to_rule`, the one
//! `make_rule.rs` uses), so they have the shape of a rule made from a
//! Connections row: an exact, case-sensitive `process.path`, and for a host a
//! list that ANDs it with `dest.host` (or `dest.ip` for an address), duration
//! "always", enabled, no precedence. Only the directory rule is built here
//! (the bridge has no builder for it), as one `regexp` leaf.
//!
//! "Only these destinations" is ONE rule per destination: the daemon ANDs the
//! members of a list, so two hosts in one list would match nothing.
//!
//! Names are `snitchwatch-learned-<12 hex>` (allow, directory) and
//! `snitchwatch-learned-block-<12 hex>` (block), the hash being FNV-1a over
//! the scope (kind, path or pattern, host), so a name is stable and tells the
//! scopes apart. NOTE for classifiers: [`ALLOW_NAME_PREFIX`] is a prefix of
//! [`BLOCK_NAME_PREFIX`]; test for the block prefix first. Neither is a
//! reserved prefix (`snitchwatch-allow-`, `snitchwatch-default-`, ...).
//!
//! `created` is 0 (the daemon stamps its own), which keeps a payload a pure
//! function of its inputs.

use std::net::IpAddr;

use snitchwatch_bridge::cache::connections::Verdict;
use snitchwatch_bridge::rule_wire::rule_to_wire;
use snitchwatch_bridge::translator::verdict::verdict_to_rule;
use snitchwatch_bridge::ws_messages::{VerdictDuration, VerdictScope};
use snitchwatch_proto::protocol::{Connection, Operator, Rule};

use super::directory::directory_rule;
use super::paths::selectability;
use super::types::{Choice, DirectoryRule, NonSelectable};
use crate::learned::Program;

/// Name prefix of the rules that allow.
pub const ALLOW_NAME_PREFIX: &str = "snitchwatch-learned-";
/// Name prefix of the rules that block (starts with [`ALLOW_NAME_PREFIX`]).
pub const BLOCK_NAME_PREFIX: &str = "snitchwatch-learned-block-";

const DESCRIPTION: &str = "snitchwatch learned rule";
/// The longest destination kept (a DNS name).
const MAX_HOST_BYTES: usize = 253;

/// One rule, as the bridge would receive it in an `AddRule`.
#[derive(Debug, Clone, PartialEq)]
pub struct RulePayload {
    pub rule: Rule,
}

impl RulePayload {
    pub fn name(&self) -> &str {
        &self.rule.name
    }

    /// The wire rule (`AddRule`'s `rule`).
    pub fn to_wire(&self) -> serde_json::Value {
        rule_to_wire(&self.rule)
    }
}

/// Why a choice makes no rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildRefusal {
    /// No rule can be made for the program.
    NonSelectable(NonSelectable),
    /// "Only these destinations" with none.
    NoHosts,
    /// A destination that cannot be a rule's host.
    BadHost,
    /// A directory rule that is not the program's own.
    NoDirectoryRule,
    /// The choice makes no rule (it is a reason, not an action).
    NotOffered,
}

/// FNV-1a, 64 bit.
pub(super) fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// 12 hex digits naming a scope made of `parts`. Each part is
/// length-prefixed, so `["a", "bc"]` and `["ab", "c"]` differ.
pub(super) fn scope_hash(parts: &[&str]) -> String {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    format!("{:012x}", fnv1a64(&bytes) >> 16)
}

/// Whether `host` can be the destination of a rule: a name or an address,
/// with no space or control character.
pub(super) fn is_usable_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST_BYTES
        && !host.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn conn(path: &str) -> Connection {
    Connection {
        process_path: path.to_string(),
        ..Connection::default()
    }
}

fn learned(
    verdict: Verdict,
    scope: VerdictScope,
    conn: &Connection,
    name: String,
) -> Result<RulePayload, BuildRefusal> {
    let made = verdict_to_rule(verdict, VerdictDuration::Always, scope, conn, 0)
        .map_err(|_| BuildRefusal::NonSelectable(NonSelectable::NotBindable))?;
    Ok(RulePayload {
        rule: Rule {
            name,
            description: DESCRIPTION.to_string(),
            ..made
        },
    })
}

fn host_rule(path: &str, host: &str) -> Result<RulePayload, BuildRefusal> {
    if !is_usable_host(host) {
        return Err(BuildRefusal::BadHost);
    }
    let mut c = conn(path);
    // An address is matched as `dest.ip`: a `dest.host` rule for an address
    // never matches, the daemon's host being empty then.
    if host.parse::<IpAddr>().is_ok() {
        c.dst_ip = host.to_string();
    } else {
        c.dst_host = host.to_string();
    }
    let name = format!(
        "{ALLOW_NAME_PREFIX}{}",
        scope_hash(&["allow-host", path, &host.to_ascii_lowercase()])
    );
    learned(Verdict::Allow, VerdictScope::ThisHost, &c, name)
}

fn directory_payload(
    program: &Program,
    wanted: &DirectoryRule,
) -> Result<RulePayload, BuildRefusal> {
    let own = directory_rule(program).ok_or(BuildRefusal::NoDirectoryRule)?;
    if &own != wanted {
        return Err(BuildRefusal::NoDirectoryRule);
    }
    let made = verdict_to_rule(
        Verdict::Allow,
        VerdictDuration::Always,
        VerdictScope::AnyHost,
        &conn(program.raw_path.as_deref().unwrap_or_default()),
        0,
    )
    .map_err(|_| BuildRefusal::NoDirectoryRule)?;
    Ok(RulePayload {
        rule: Rule {
            name: format!(
                "{ALLOW_NAME_PREFIX}{}",
                scope_hash(&["allow-directory", &own.pattern])
            ),
            description: DESCRIPTION.to_string(),
            operator: Some(Operator {
                r#type: "regexp".to_string(),
                operand: "process.path".to_string(),
                data: own.pattern,
                sensitive: own.sensitive,
                list: Vec::new(),
            }),
            ..made
        },
    })
}

/// The rules `choice` makes for `program`: one, or one per destination of
/// "only these destinations". Refused when the choice is not one that makes
/// a rule, or does not fit the program (the checks are repeated here, so a
/// choice built by hand cannot get past them).
pub fn rules_for(choice: &Choice, program: &Program) -> Result<Vec<RulePayload>, BuildRefusal> {
    if matches!(choice, Choice::NonSelectable(_)) {
        return Err(BuildRefusal::NotOffered);
    }
    let path = selectability(program).map_err(BuildRefusal::NonSelectable)?;
    match choice {
        Choice::AllowAny => {
            let name = format!("{ALLOW_NAME_PREFIX}{}", scope_hash(&["allow-any", path]));
            Ok(vec![learned(
                Verdict::Allow,
                VerdictScope::AnyHost,
                &conn(path),
                name,
            )?])
        }
        Choice::Block => {
            let name = format!("{BLOCK_NAME_PREFIX}{}", scope_hash(&["block", path]));
            Ok(vec![learned(
                Verdict::Deny,
                VerdictScope::AnyHost,
                &conn(path),
                name,
            )?])
        }
        Choice::AllowHosts(hosts) if hosts.is_empty() => Err(BuildRefusal::NoHosts),
        Choice::AllowHosts(hosts) => hosts.iter().map(|host| host_rule(path, host)).collect(),
        Choice::DirectoryRule(wanted) => Ok(vec![directory_payload(program, wanted)?]),
        Choice::NonSelectable(_) => Err(BuildRefusal::NotOffered),
    }
}
