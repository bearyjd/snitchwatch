//! A rule's conditions as a conjunction of **atoms**, and when one atom
//! provably implies another (P2.6 Part 2, plan
//! `docs/superpowers/plans/2026-10-08-rule-insights.md`).
//!
//! `A` *covers* `B` when every connection `B` matches also matches `A`: each
//! atom of `A` is implied by some atom of `B`. `B`'s extra atoms only narrow
//! `B`, so they never hurt. The proof is conservative on purpose; **no proof
//! means "don't know", never "doesn't cover"**, and the only consumer
//! (`shadow`) claims something only when there is one.
//!
//! What is modelled, because the daemon's comparison is known exactly:
//!
//! - a `simple` or `regexp` condition on a text operand (`process.path`,
//!   `process.command`, `process.id`, `dest.host`, `dest.ip`, `dest.port`,
//!   `source.ip`, `source.port`, `user.id`, `protocol`);
//! - a `network` condition on `dest.network` / `source.network`;
//! - the `true` operand (no atom: it always matches).
//!
//! Everything else is **opaque**: `lists.*`, `process.hash.*`, `user.name`,
//! `iface.*`, `process.env.*`, `process.parent.path`, a condition type or
//! pairing the daemon doesn't load, a list inside a list, an empty list,
//! an operator that can't be read. A rule with an opaque atom never covers
//! anything. (An opaque atom on the *other* rule is harmless: it can only
//! narrow it.)
//!
//! Implications ([`Proof::Exact`] unless noted):
//!
//! - **identical** atoms (same operand, type, data, case rule);
//! - **simple ⇒ simple**: the goal is insensitive and the two texts are equal
//!   under Go's simple case folding (`strings.EqualFold`), or the goal is
//!   sensitive and so is the premise, with the same text. An insensitive
//!   premise never implies a sensitive goal;
//! - **simple ⇒ regexp** ([`Proof::Engine`]): the goal's pattern matches the
//!   premise's literal. For an insensitive premise that proves nothing about
//!   its case variants unless the goal is insensitive too, and then only for
//!   an ASCII literal without an `s`: Go folds U+017F (long s) with `s`, but
//!   lowercasing leaves it alone, so such a subject can match the literal and
//!   miss the pattern;
//! - **network ⇒ network** and **`dest.ip`/`source.ip` literal ⇒ network**:
//!   containment between literal IPv4 CIDRs (host bits masked, as
//!   `net.ParseCIDR` does) and a canonically written IPv4 address. An alias
//!   (`LAN`) is whatever the daemon host's alias file says, so it implies only
//!   itself; IPv6 networks likewise.

use std::cell::OnceCell;
use std::net::Ipv4Addr;

use crate::rules::row_store::Rule;
use crate::rules::simulator::compare::{simple_cmp, Regexp};
use crate::rules::simulator::operator::{parse, Kind, Leaf, Operator};

/// How strong a proof is. `Exact` rests on comparisons reproduced exactly;
/// `Engine` also on the simulator's regular-expression engine, which differs
/// from the daemon's (Go RE2) for rare constructs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Proof {
    Exact,
    Engine,
}

const TEXT_OPERANDS: &[&str] = &[
    "process.path",
    "process.command",
    "process.id",
    "dest.host",
    "dest.ip",
    "dest.port",
    "source.ip",
    "source.port",
    "user.id",
    "protocol",
];
const NETWORK_OPERANDS: &[&str] = &["dest.network", "source.network"];

struct Atom {
    operand: String,
    kind: Kind,
    data: String,
    sensitive: bool,
    /// Compiled on first use as a goal.
    regexp: OnceCell<Option<Regexp>>,
}

impl Atom {
    /// `None` for a condition the analysis doesn't model.
    fn from_leaf(leaf: &Leaf) -> Option<Self> {
        let operand = leaf.operand();
        let modelled = match leaf.kind() {
            Kind::Simple | Kind::Regexp => TEXT_OPERANDS.contains(&operand),
            Kind::Network => NETWORK_OPERANDS.contains(&operand),
            Kind::Lists => false,
        };
        modelled.then(|| Self {
            operand: operand.to_string(),
            kind: leaf.kind(),
            data: leaf.data().to_string(),
            sensitive: leaf.sensitive(),
            regexp: OnceCell::new(),
        })
    }

    fn identical(&self, other: &Self) -> bool {
        self.operand == other.operand
            && self.kind == other.kind
            && self.data == other.data
            && self.sensitive == other.sensitive
    }

    fn regexp(&self) -> Option<&Regexp> {
        self.regexp
            .get_or_init(|| Regexp::compile(&self.data, self.sensitive).ok())
            .as_ref()
    }
}

/// A rule's conditions, all of which must hold.
pub struct Conjunction {
    atoms: Vec<Atom>,
    /// Some condition isn't modelled: this rule covers nothing.
    opaque: bool,
}

impl Conjunction {
    pub fn from_rule(rule: &Rule) -> Self {
        let mut conjunction = Self {
            atoms: Vec::new(),
            opaque: false,
        };
        match parse(&rule.operator) {
            Operator::List(members) if members.is_empty() => conjunction.opaque = true,
            Operator::List(members) => {
                for member in &members {
                    conjunction.add(member);
                }
            }
            single => conjunction.add(&single),
        }
        conjunction
    }

    fn add(&mut self, operator: &Operator) {
        match operator {
            Operator::True => {}
            Operator::Leaf(leaf) => match Atom::from_leaf(leaf) {
                Some(atom) => self.atoms.push(atom),
                None => self.opaque = true,
            },
            // A list in a list, an unreadable shape.
            Operator::List(_) | Operator::Unsupported { .. } => self.opaque = true,
        }
    }

    /// Whether `self` provably matches every connection `other` matches, and
    /// how strongly that is proven.
    pub fn covers(&self, other: &Self) -> Option<Proof> {
        if self.opaque {
            return None;
        }
        let mut weakest = Proof::Exact;
        for goal in &self.atoms {
            let strongest = other
                .atoms
                .iter()
                .filter_map(|premise| implies(premise, goal))
                .min()?;
            weakest = weakest.max(strongest);
        }
        Some(weakest)
    }

    /// The rule can cover another rule (it has no opaque condition).
    pub fn is_modelled(&self) -> bool {
        !self.opaque
    }
}

/// Whether every connection satisfying `premise` satisfies `goal`.
fn implies(premise: &Atom, goal: &Atom) -> Option<Proof> {
    if premise.identical(goal) {
        return Some(Proof::Exact);
    }
    match (premise.kind, goal.kind) {
        (Kind::Simple, Kind::Simple) if premise.operand == goal.operand => {
            simple_implies_simple(premise, goal).then_some(Proof::Exact)
        }
        (Kind::Simple, Kind::Regexp) if premise.operand == goal.operand => {
            simple_implies_regexp(premise, goal).then_some(Proof::Engine)
        }
        (Kind::Simple, Kind::Network) => {
            literal_ip_in_network(premise, goal).then_some(Proof::Exact)
        }
        (Kind::Network, Kind::Network) if premise.operand == goal.operand => {
            network_in_network(&premise.data, &goal.data).then_some(Proof::Exact)
        }
        _ => None,
    }
}

fn simple_implies_simple(premise: &Atom, goal: &Atom) -> bool {
    if goal.sensitive {
        // The only subject is the premise's text, and only if it is exact.
        premise.sensitive && premise.data == goal.data
    } else {
        simple_cmp(&premise.data, &goal.data, false)
    }
}

fn simple_implies_regexp(premise: &Atom, goal: &Atom) -> bool {
    let Some(regexp) = goal.regexp() else {
        return false;
    };
    if premise.sensitive {
        return regexp.is_match(&premise.data);
    }
    // The case variants of the premise's literal must all match: the goal
    // lowercases its subject, so it has to be insensitive, and see only
    // variants that lowercase to the literal's own lowercase (see the module
    // docs for the long s).
    !goal.sensitive
        && premise.data.is_ascii()
        && !premise.data.contains(['s', 'S'])
        && regexp.is_match(&premise.data)
}

/// `dest.ip` / `source.ip` literal inside the matching `*.network` goal.
fn literal_ip_in_network(premise: &Atom, goal: &Atom) -> bool {
    let pair = matches!(
        (premise.operand.as_str(), goal.operand.as_str()),
        ("dest.ip", "dest.network") | ("source.ip", "source.network")
    );
    if !pair {
        return false;
    }
    let (Some(ip), Some(net)) = (canonical_ipv4(&premise.data), V4Net::parse(&goal.data)) else {
        return false;
    };
    net.contains(ip)
}

fn network_in_network(premise: &str, goal: &str) -> bool {
    let (Some(inner), Some(outer)) = (V4Net::parse(premise), V4Net::parse(goal)) else {
        return false;
    };
    inner.prefix >= outer.prefix && outer.contains(inner.addr)
}

/// `net.IP.String()` of an IPv4 address: the dotted quad, no leading zeros.
/// Anything else is a text the daemon's subject can never equal (or an IPv6
/// form this analysis doesn't compare).
fn canonical_ipv4(text: &str) -> Option<u32> {
    let ip: Ipv4Addr = text.parse().ok()?;
    (ip.to_string() == text).then(|| u32::from(ip))
}

/// A literal IPv4 network, host bits masked (`net.ParseCIDR`).
struct V4Net {
    addr: u32,
    prefix: u8,
}

impl V4Net {
    fn parse(text: &str) -> Option<Self> {
        let (addr, prefix) = text.split_once('/')?;
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let prefix: u8 = prefix.parse().ok().filter(|p| *p <= 32)?;
        let addr: Ipv4Addr = addr.parse().ok()?;
        Some(Self {
            addr: u32::from(addr) & mask(prefix),
            prefix,
        })
    }

    fn contains(&self, ip: u32) -> bool {
        ip & mask(self.prefix) == self.addr
    }
}

fn mask(prefix: u8) -> u32 {
    u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0)
}
