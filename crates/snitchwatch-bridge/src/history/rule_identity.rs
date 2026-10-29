//! Connection history, PR 4a: event-time rule identity (design D7), as pure
//! functions over a daemon [`Rule`]. No database, no persistence, no call
//! site: PR 4b stores what [`identify`] returns.
//!
//! The rule is never copied. A **recursive allowlist** builds the redacted
//! canonical form, and the hash is taken over that form only, so no digest of
//! a withheld value (a command-line fragment, an environment value) exists.
//!
//! * Kept: name, action, duration, enabled, precedence, `nolog`, and per
//!   operator the type, the operand kind, `sensitive` and the structure.
//! * Operand *values* are kept only for [`KEPT_OPERANDS`] and `lists.*`.
//!   Every other operand's value is withheld. The operand *name* of an
//!   unknown kind or of `process.env.<NAME>` is withheld too (it is user
//!   text), so those encode as fixed tokens.
//! * Dropped: description, creation time, and the JSON copy of a `list`
//!   operator's members in its `data`.
//!
//! Raw `process.path` / `dest.host` values are the user's own rule text and
//! are recorded as written (the stated limit in D4).
//!
//! Failing closed: a rule beyond [`MAX_OPERATOR_DEPTH`], [`MAX_NODES`],
//! [`MAX_OPERATOR_LIST_LEN`] members per list or [`caps::SNAPSHOT`] canonical
//! bytes has no identity ([`IdentityError`]), and recursion stops at the
//! depth bound, so a hostile nested rule cannot overflow the stack.

use sha2::{Digest, Sha256};
use snitchwatch_proto::protocol::{Operator, Rule};

use super::caps::{RULE_HASH, RULE_NAME, SNAPSHOT};
use crate::cache::rules::{MAX_OPERATOR_DEPTH, MAX_OPERATOR_LIST_LEN};
use crate::rule_name::{BLOCKLIST_RULE_NAME_PREFIX, CURATED_DEFAULT_RULE_NAME_PREFIX};

/// Total operators one rule may have.
pub const MAX_NODES: usize = 256;

/// Operand kinds whose value is recorded (D7).
const KEPT_OPERANDS: [&str; 6] = [
    "process.path",
    "dest.host",
    "dest.ip",
    "dest.port",
    "protocol",
    "user.id",
];

/// Backed by a directory of list files: the value (the directory) is kept,
/// the contents are not recorded.
const LIST_OPERANDS: [&str; 5] = [
    "lists.domains",
    "lists.domains_regexp",
    "lists.ips",
    "lists.nets",
    "lists.hashes.md5",
];

/// Known operand kinds whose value is withheld; the kind name is kept.
const WITHHELD_OPERANDS: [&str; 14] = [
    "process.id",
    "process.parent.path",
    "process.command",
    "process.hash.md5",
    "process.hash.sha1",
    "user.name",
    "source.ip",
    "source.port",
    "source.network",
    "dest.network",
    "iface.in",
    "iface.out",
    // The two below carry no value of their own (see `Value::None`).
    "true",
    "list",
];

const ENV_PREFIX: &str = "process.env.";
const ENV_TOKEN: &str = "process.env.*";
const UNKNOWN: &str = "[unknown]";
const OPERATOR_TYPES: [&str; 5] = ["simple", "regexp", "network", "lists", "list"];
const ACTIONS: [&str; 3] = ["allow", "deny", "reject"];
const MAX_DURATION: usize = 32;
const VERSION: &[u8] = b"SWRI\x01";

/// Why a rule has no identity. Never carries rule text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("rule name longer than {RULE_NAME} bytes")]
    NameTooLong,
    #[error("operators nested deeper than {MAX_OPERATOR_DEPTH}")]
    TooDeep,
    #[error("more than {MAX_NODES} operators")]
    TooManyOperators,
    #[error("an operator list longer than {MAX_OPERATOR_LIST_LEN}")]
    ListTooLong,
    #[error("redacted form longer than {SNAPSHOT} bytes")]
    TooLarge,
}

/// `events.rule_class`, derived from the rule-name prefixes already in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleClass {
    SnitchwatchDefault,
    Blocklist,
    User,
}

impl RuleClass {
    pub fn of(name: &str) -> Self {
        if name.starts_with(CURATED_DEFAULT_RULE_NAME_PREFIX) {
            Self::SnitchwatchDefault
        } else if name.starts_with(BLOCKLIST_RULE_NAME_PREFIX) {
            Self::Blocklist
        } else {
            Self::User
        }
    }

    /// Fits `caps::RULE_CLASS`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SnitchwatchDefault => "snitchwatch-default",
            Self::Blocklist => "z00-blocklist",
            Self::User => "user",
        }
    }
}

/// What the UI must say about a rule's fidelity (D7). Derived from the
/// redacted form only, so equal hashes always carry equal qualifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qualifiers {
    /// A `lists.*` operator: the hash does not capture the list contents.
    pub list_contents_not_recorded: bool,
    /// Operators whose value is withheld; change detection covers only the
    /// visible part of such a rule.
    pub withheld_conditions: u32,
}

impl Qualifiers {
    pub fn partly_not_recorded(&self) -> bool {
        self.withheld_conditions > 0
    }
}

/// The identity of one rule at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleIdentity {
    pub hash: [u8; RULE_HASH],
    /// The redacted canonical bytes the hash covers.
    pub canonical: Vec<u8>,
    pub class: RuleClass,
    pub qualifiers: Qualifiers,
}

/// Redact, encode and hash `rule`.
pub fn identify(rule: &Rule) -> Result<RuleIdentity, IdentityError> {
    if rule.name.len() > RULE_NAME {
        return Err(IdentityError::NameTooLong);
    }
    let mut enc = Encoder::default();
    enc.bytes(VERSION);
    enc.text(&rule.name)?;
    enc.text(allowed(&rule.action, &ACTIONS))?;
    enc.text(duration(&rule.duration))?;
    enc.bytes(&[
        u8::from(rule.enabled),
        u8::from(rule.precedence),
        u8::from(rule.nolog),
        u8::from(rule.operator.is_some()),
    ]);
    if let Some(op) = &rule.operator {
        enc.operator(op, 0)?;
    }
    let digest = Sha256::digest(&enc.out);
    let mut hash = [0u8; RULE_HASH];
    hash.copy_from_slice(&digest[..RULE_HASH]);
    Ok(RuleIdentity {
        hash,
        class: RuleClass::of(&rule.name),
        qualifiers: Qualifiers {
            list_contents_not_recorded: enc.lists,
            withheld_conditions: enc.withheld,
        },
        canonical: enc.out,
    })
}

fn allowed<'a>(value: &'a str, set: &[&str]) -> &'a str {
    if set.contains(&value) {
        value
    } else {
        UNKNOWN
    }
}

fn duration(value: &str) -> &str {
    let plain = value.len() <= MAX_DURATION
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b' ');
    if plain {
        value
    } else {
        UNKNOWN
    }
}

/// What the canonical form records of an operator's value.
enum Value<'a> {
    Kept(&'a str),
    Withheld,
    /// Structural operator (`true`, `list`): nothing to record or withhold.
    None,
}

#[derive(Default)]
struct Encoder {
    out: Vec<u8>,
    nodes: usize,
    withheld: u32,
    lists: bool,
}

impl Encoder {
    fn bytes(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
    }

    /// A u32 big-endian length, then the bytes: no two field sequences share
    /// an encoding. Checked before the copy, so output never exceeds the cap.
    fn text(&mut self, s: &str) -> Result<(), IdentityError> {
        let len = u32::try_from(s.len()).map_err(|_| IdentityError::TooLarge)?;
        if self.out.len() + 4 + s.len() > SNAPSHOT {
            return Err(IdentityError::TooLarge);
        }
        self.bytes(&len.to_be_bytes());
        self.bytes(s.as_bytes());
        Ok(())
    }

    fn operator(&mut self, op: &Operator, depth: usize) -> Result<(), IdentityError> {
        if depth > MAX_OPERATOR_DEPTH {
            return Err(IdentityError::TooDeep);
        }
        if op.list.len() > MAX_OPERATOR_LIST_LEN {
            return Err(IdentityError::ListTooLong);
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(IdentityError::TooManyOperators);
        }
        let (kind, value) = classify(op);
        self.text(allowed(&op.r#type, &OPERATOR_TYPES))?;
        self.text(&kind)?;
        self.bytes(&[u8::from(op.sensitive)]);
        match value {
            Value::Kept(data) => {
                self.bytes(&[1]);
                self.text(data)?;
            }
            Value::Withheld => {
                self.bytes(&[0]);
                self.withheld += 1;
            }
            Value::None => self.bytes(&[2]),
        }
        self.lists |= LIST_OPERANDS.contains(&kind.as_str());
        let members = u32::try_from(op.list.len()).map_err(|_| IdentityError::ListTooLong)?;
        self.bytes(&members.to_be_bytes());
        op.list.iter().try_for_each(|m| self.operator(m, depth + 1))
    }
}

/// The operand token the canonical form uses, and what happens to the value.
fn classify(op: &Operator) -> (String, Value<'_>) {
    let kind = op.operand.as_str();
    if KEPT_OPERANDS.contains(&kind) || LIST_OPERANDS.contains(&kind) {
        (kind.to_owned(), Value::Kept(&op.data))
    } else if kind == "true" || kind == "list" {
        (kind.to_owned(), Value::None)
    } else if WITHHELD_OPERANDS.contains(&kind) {
        (kind.to_owned(), Value::Withheld)
    } else if kind.starts_with(ENV_PREFIX) {
        (ENV_TOKEN.to_owned(), Value::Withheld)
    } else {
        (UNKNOWN.to_owned(), Value::Withheld)
    }
}
