//! Qt-free rule-match **simulator** (Little-Snitch-parity "rule-match
//! diagnostics" — the interactive counterpart to
//! [`super::row_store::found_rule_json`]'s "show me the rule that already
//! decided this connection").
//!
//! Given a candidate connection, [`simulate`] evaluates the *cached* rule
//! list ([`RulesStore`]) the way opensnitchd v1.8.0 does:
//! `rule.Loader.FindFirstMatch` and `Loader.sortRules`
//! (`vendor/opensnitch/daemon/rule/loader.go`) for the order, and
//! `Operator.Match`/`Operator.Compile` (`.../rule/operator.go`) for each
//! condition.
//!
//!   * **Order**: enabled rules only, sorted by **name** byte-wise
//!     (`sort.Strings`) — the store's own order isn't trusted, because
//!     `UpdateRules` appends. Disabled rules are skipped entirely.
//!   * **First-match-wins is not "first true wins"**: iterate in order; on
//!     every match remember the rule; if its action is `deny`/`reject` **or**
//!     its `precedence` flag is set, stop there. Otherwise keep going — a
//!     *later* matching `allow` silently replaces an earlier one, so without
//!     a stop rule the *last* matching allow wins.
//!   * **`list` is AND** with short-circuit (`res = res && child.Match()`),
//!     starting from `true`: an empty list matches. A list *inside* a list
//!     is not simulated: opensnitchd compiles only one level of members, and
//!     rules that arrive over gRPC lose a nested list's members, so what the
//!     daemon does with one is a panic or a match-everything, not AND.
//!   * **Dispatch** is by operand, in `Match`'s order; the operator *type*
//!     only picks the comparison. An operand `Match` has no branch for is
//!     simply `false`.
//!   * **Comparisons**: `simple` is `strings.EqualFold` (Unicode case
//!     folding) unless `sensitive`; `regexp` lowercases both the pattern
//!     source and the subject unless `sensitive`, and is unanchored; `network`
//!     is CIDR membership or one of the daemon's aliases. See [`compare`] and
//!     [`network`].
//!
//! ## Every operand opensnitchd v1.8.0 evaluates
//!
//! `process.path`, `process.parent.path` (any ancestor), `process.command`,
//! `process.id`, `process.env.NAME`, `process.hash.md5`/`sha1`, `user.id`,
//! `source.ip`/`port`/`network`, `dest.ip`/`host`/`port`/`network`,
//! `protocol`, `iface.in`/`out`, `list` and `true`. Hash conditions follow the
//! daemon's quirk: they match every program while checksums are off, and a
//! program with no checksum matches them even with checksums on — which is why
//! such a verdict carries a warning.
//!
//! ## Unknown inputs and operands that can't be simulated
//!
//! An input is `None` when the caller doesn't know it ("unknown", not
//! "empty"). A condition that needs one is **not evaluated** — never guessed
//! at, never a match — and the result lists which rule and which input. A
//! rule's conditions combine three-valued: one that is certainly false decides
//! the rule whatever else is unknown.
//!
//! Some conditions can't be simulated at all, each with its own reason:
//! `user.name` (opensnitchd resolves it to a uid on the daemon host when the
//! rule loads), `lists.*` (the list files live on the daemon host) and a list
//! nested in a list. They are reported in [`SimulationResult::unsupported_operands`], never as a
//! match.
//!
//! Every result is a [`SimulationResult`] — a simulation over cached data, not
//! a live daemon verdict — and the Rules page labels it so.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use self::operator::{evaluate, parse, Gap, Truth};
use super::row_store::{Rule, RulesStore};

mod compare;
mod form;
mod network;
mod operator;
#[cfg(test)]
mod tests;

pub use form::SimulationForm;

/// The candidate connection to evaluate against the cached rule set.
///
/// The destination host, port and protocol are always known (a blank host is
/// the empty `DstHost` of a bare-IP connection). Every other field is `None`
/// when **unknown** — not "empty" — so conditions on it are reported as not
/// evaluated instead of being guessed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimulationInput {
    pub process_path: Option<String>,
    pub dest_host: String,
    pub dest_port: u16,
    /// As opensnitchd names it: `tcp`, `tcp6`, `udp`, `udp6`, `udplite`,
    /// `sctp`, `icmp`, `icmp6`... Matched like the daemon's `protocol`
    /// operand (case-folded unless the rule is `sensitive`).
    pub protocol: String,
    /// Paths of the process's ancestors, nearest first. `Some(vec![])` is a
    /// process with no parent.
    pub parent_paths: Option<Vec<String>>,
    /// The arguments joined with single spaces, `argv[0]` first.
    pub command: Option<String>,
    pub pid: Option<u32>,
    pub uid: Option<u32>,
    /// The whole environment: a variable not in it is unset (compares as
    /// `""`).
    pub env: Option<BTreeMap<String, String>>,
    pub src_ip: Option<String>,
    pub src_port: Option<u16>,
    pub dest_ip: Option<String>,
    pub iface_in: Option<String>,
    pub iface_out: Option<String>,
    /// The program's recorded checksums by algorithm. `Some(empty)` is a
    /// program with none recorded; `None` is unknown. Only read when
    /// `checksums_enabled` is `Some(true)`.
    pub checksums: Option<BTreeMap<String, String>>,
    /// Whether opensnitchd computes checksums (`Rules.EnableChecksums`).
    pub checksums_enabled: Option<bool>,
}

/// An operand the simulator can't evaluate at all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unsupported {
    pub operand: String,
    pub reason: String,
}

/// A condition that needs an input the caller left unknown.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unevaluated {
    /// The rule's display name.
    pub rule: String,
    pub operand: String,
    /// The input it lacked, e.g. `"user ID"`.
    pub missing: String,
}

/// Outcome of [`simulate`]. Always labelled as a simulation — never a live
/// daemon verdict — per this module's doc comment.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationResult {
    /// `Some(name)` (the rule's display name) when a rule matched (mirrors
    /// `Loader.FindFirstMatch`'s possibly-nil `match`); `None` means no enabled rule matched at all, so
    /// opensnitchd's own configured default action would apply instead — this
    /// module has no visibility into that default, so it just reports "no
    /// match".
    pub matched_rule: Option<String>,
    pub action: Option<String>,
    /// The deciding rule's row in the rules list (the store's order), or
    /// `None` when nothing matched.
    pub precedence: Option<usize>,
    /// Operands that can't be simulated (`user.name`, `lists.*`...) in rules
    /// that could have changed the verdict. The verdict assumes those rules
    /// did not match.
    pub unsupported_operands: Vec<Unsupported>,
    /// Conditions in rules that could have changed the verdict but needed an
    /// input left unknown. The verdict assumes those rules did not match.
    pub unevaluated: Vec<Unevaluated>,
    /// Notes on how the deciding rule matched (hash conditions).
    pub warnings: Vec<String>,
}

/// Whether a matching rule ends the scan (`FindFirstMatch`): `reject`,
/// `deny` or `precedence`. The action is compared as opensnitchd does,
/// exactly.
fn stops_scan(rule: &Rule) -> bool {
    rule.precedence || rule.action == "deny" || rule.action == "reject"
}

/// Simulate matching `input` against `store`'s cached, enabled rules using
/// opensnitchd's exact precedence semantics (see module docs).
pub fn simulate(store: &RulesStore, input: &SimulationInput) -> SimulationResult {
    let mut active: Vec<&Rule> = store.rules().iter().filter(|r| r.enabled).collect();
    active.sort_by(|a, b| a.name.cmp(&b.name));

    let mut decider: Option<(usize, &Rule, Vec<&'static str>)> = None;
    let mut undecided: Vec<(usize, &Rule, Vec<Gap>)> = Vec::new();
    for (position, rule) in active.iter().enumerate() {
        let outcome = evaluate(&parse(&rule.operator), input);
        match outcome.truth {
            Truth::No => {}
            Truth::Unknown => undecided.push((position, rule, outcome.gaps)),
            Truth::Yes => {
                decider = Some((position, rule, outcome.warnings));
                if stops_scan(rule) {
                    break;
                }
            }
        }
    }

    // A rule whose conditions couldn't be decided is treated as a non-match.
    // Report it only if it could have changed the verdict: a stop rule
    // anywhere in the scan could have ended it, but an allow earlier than the
    // deciding rule would just have been replaced by it.
    let decider_position = decider.as_ref().map(|(position, _, _)| *position);
    let mut unsupported = BTreeSet::new();
    let mut unevaluated = Vec::new();
    let mut seen = BTreeSet::new();
    for (position, rule, gaps) in undecided {
        let could_matter = stops_scan(rule) || decider_position.is_none_or(|d| position > d);
        if !could_matter {
            continue;
        }
        for gap in gaps {
            match gap {
                Gap::Unsupported { operand, reason } => {
                    unsupported.insert(Unsupported {
                        operand,
                        reason: reason.to_string(),
                    });
                }
                Gap::Unevaluated { operand, missing } => {
                    let entry = Unevaluated {
                        rule: rule.shown_name().to_string(),
                        operand,
                        missing: missing.to_string(),
                    };
                    if seen.insert(entry.clone()) {
                        unevaluated.push(entry);
                    }
                }
            }
        }
    }

    let unsupported_operands = unsupported.into_iter().collect();
    match decider {
        Some((_, rule, warnings)) => SimulationResult {
            matched_rule: Some(rule.shown_name().to_string()),
            action: Some(rule.normalized_action().to_string()),
            precedence: store.index_of(&rule.name),
            unsupported_operands,
            unevaluated,
            warnings: warnings.into_iter().map(str::to_string).collect(),
        },
        None => SimulationResult {
            matched_rule: None,
            action: None,
            precedence: None,
            unsupported_operands,
            unevaluated,
            warnings: Vec::new(),
        },
    }
}
