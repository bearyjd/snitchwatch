//! One rule's operator tree: parsing the wire JSON and evaluating it the way
//! opensnitchd's `Operator.Match` does
//! (`vendor/opensnitch/daemon/rule/operator.go`).
//!
//! Every leaf ends in one of three states: it matches, it doesn't, or it
//! **can't be decided** — its subject is an input the caller left unknown, or
//! the operand is one this simulator can't evaluate. An undecided leaf is
//! never guessed at; see [`evaluate_list`] for how it combines with the rest.

use serde_json::Value;

use super::{compare, network, SimulationInput};

// What a leaf lacked: shown to the user as "needs <this>".
const PROCESS_PATH: &str = "process path";
const PARENT_PATHS: &str = "parent process paths";
const COMMAND: &str = "process command line";
const PID: &str = "process ID";
const UID: &str = "user ID";
const ENV: &str = "process environment";
const SRC_IP: &str = "source IP";
const SRC_PORT: &str = "source port";
const DEST_IP: &str = "destination IP";
const IFACE_IN: &str = "inbound interface";
const IFACE_OUT: &str = "outbound interface";
const CHECKSUM: &str = "program checksum";
const BAD_SRC_IP: &str = "a valid source IP address (the text entered isn't one)";
const BAD_DEST_IP: &str = "a valid destination IP address (the text entered isn't one)";

// Why an operand can't be simulated at all.
const USER_NAME: &str = "resolved to a numeric user id on the daemon host when the rule loads, \
     which Snitchwatch can't look up";
const LISTS: &str = "can't simulate list contents (the lists are files on the daemon host)";
const BAD_PAIRING: &str = "opensnitchd can't apply this condition type to this operand (the \
     daemon would fail on a match), so it isn't simulated";
const UNKNOWN_NETWORK: &str = "not a CIDR or one of the default aliases (LAN, MULTICAST); a \
     custom alias from the daemon host's network_aliases.json can't be resolved here";
const NESTED_LIST: &str = "a list inside a list: opensnitchd compiles only one level of members \
     (and drops a nested list's members for rules sent over gRPC), so it fails or matches \
     everything instead of ANDing them";
const UNKNOWN_TYPE: &str = "a condition type opensnitchd 1.8.0 doesn't load";
const LEGACY_SHAPE: &str = "a legacy rule shape the simulator doesn't read";
const UNREADABLE: &str = "the condition's shape isn't recognised";

// Notes on a verdict that leans on how opensnitchd treats hash conditions.
const HASH_OFF: &str = "Hash conditions match every program while checksums are off.";
const HASH_UNKNOWN: &str = "Hash conditions may match every program: it isn't known whether \
     checksums are on, and while they are off a hash condition always matches.";
const HASH_NONE_RECORDED: &str =
    "A program with no recorded checksum matches hash conditions, even with checksums on.";

/// The characters `core.Trim` strips from the variable name in a
/// `process.env.<NAME>` operand.
fn is_env_trim(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\t' | ' ')
}

// ---- parsing ----------------------------------------------------------------

pub(super) enum Operator {
    /// The `true` operand.
    True,
    /// Every member must match.
    List(Vec<Operator>),
    Leaf(Leaf),
    /// Recognised as an operator node but not one that can be evaluated.
    Unsupported {
        operand: String,
        reason: &'static str,
    },
}

pub(super) struct Leaf {
    kind: Kind,
    operand: String,
    data: String,
    sensitive: bool,
}

/// The operator `type`, which decides *how* the operand's value is compared.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Simple,
    Regexp,
    Network,
    Lists,
}

/// Read one operator from a rule's `operator` JSON: the flat shape opensnitchd
/// uses (`{"type","operand","data","sensitive","list"}`), tolerating the
/// bridge's older externally-tagged one.
pub(super) fn parse(value: &Value) -> Operator {
    let Some(obj) = value.as_object() else {
        return unreadable("<unrecognized operator shape>");
    };

    if obj.len() == 1 {
        if let Some((tag, inner)) = obj.iter().next() {
            if inner.get("operand").is_some() || inner.get("operands").is_some() {
                return parse_tagged(tag, inner);
            }
        }
    }

    let text = |key: &str| obj.get(key).and_then(Value::as_str).unwrap_or("");
    let operand = text("operand");
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("simple");

    // `Compile` rewrites a list's operand to `list`, and `Match` tests
    // `true` and `list` before anything else.
    if kind == "list" {
        return Operator::List(parse_children(
            obj.get("list").or_else(|| obj.get("operands")),
        ));
    }
    if operand == "true" {
        return Operator::True;
    }
    if operand == "list" {
        return Operator::List(parse_children(
            obj.get("list").or_else(|| obj.get("operands")),
        ));
    }

    let kind = match kind {
        "simple" => Kind::Simple,
        "regexp" => Kind::Regexp,
        "network" => Kind::Network,
        "lists" => Kind::Lists,
        other => {
            return Operator::Unsupported {
                operand: if operand.is_empty() { other } else { operand }.to_string(),
                reason: UNKNOWN_TYPE,
            }
        }
    };
    Operator::Leaf(Leaf {
        kind,
        operand: operand.to_string(),
        data: text("data").to_string(),
        sensitive: obj
            .get("sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn parse_children(list: Option<&Value>) -> Vec<Operator> {
    list.and_then(Value::as_array)
        .map(|children| children.iter().map(parse).collect())
        .unwrap_or_default()
}

fn parse_tagged(tag: &str, inner: &Value) -> Operator {
    if tag == "list" {
        return Operator::List(parse_children(inner.get("operands")));
    }
    let operand = inner.get("operand").and_then(Value::as_str).unwrap_or(tag);
    Operator::Unsupported {
        operand: operand.to_string(),
        reason: LEGACY_SHAPE,
    }
}

fn unreadable(what: &str) -> Operator {
    Operator::Unsupported {
        operand: what.to_string(),
        reason: UNREADABLE,
    }
}

// ---- outcomes ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Truth {
    Yes,
    No,
    Unknown,
}

/// Why an operand was left undecided.
pub(super) enum Gap {
    /// An input the caller left unknown.
    Unevaluated {
        operand: String,
        missing: &'static str,
    },
    /// An operand the simulator can't evaluate at all.
    Unsupported {
        operand: String,
        reason: &'static str,
    },
}

pub(super) struct Outcome {
    pub truth: Truth,
    /// Only for [`Truth::Unknown`].
    pub gaps: Vec<Gap>,
    /// Notes on how a [`Truth::Yes`] was reached.
    pub warnings: Vec<&'static str>,
}

impl Outcome {
    fn definite(truth: Truth) -> Self {
        Self {
            truth,
            gaps: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn from_bool(matched: bool) -> Self {
        Self::definite(if matched { Truth::Yes } else { Truth::No })
    }

    fn yes_with(warning: &'static str) -> Self {
        Self {
            warnings: vec![warning],
            ..Self::definite(Truth::Yes)
        }
    }

    fn unevaluated(operand: &str, missing: &'static str) -> Self {
        Self {
            gaps: vec![Gap::Unevaluated {
                operand: operand.to_string(),
                missing,
            }],
            ..Self::definite(Truth::Unknown)
        }
    }

    fn unsupported(operand: &str, reason: &'static str) -> Self {
        Self {
            gaps: vec![Gap::Unsupported {
                operand: operand.to_string(),
                reason,
            }],
            ..Self::definite(Truth::Unknown)
        }
    }
}

// ---- evaluation ----------------------------------------------------------------

pub(super) fn evaluate(op: &Operator, input: &SimulationInput) -> Outcome {
    match op {
        Operator::True => Outcome::definite(Truth::Yes),
        Operator::Unsupported { operand, reason } => Outcome::unsupported(operand, reason),
        Operator::List(children) => evaluate_list(children, input),
        Operator::Leaf(leaf) => evaluate_leaf(leaf, input),
    }
}

/// `Operator.listMatch`: `res = res && child.Match(..)` from `true`, so an
/// empty list matches and the first non-matching member ends the evaluation
/// (later members are not looked at).
///
/// With undecided members this is three-valued AND: a definite `No` anywhere
/// decides the list (the unknowns before it can't change that, and are
/// dropped); otherwise any undecided member leaves the whole list undecided.
fn evaluate_list(children: &[Operator], input: &SimulationInput) -> Outcome {
    let mut gaps = Vec::new();
    let mut warnings: Vec<&'static str> = Vec::new();
    let mut undecided = false;
    for child in children {
        let outcome = match child {
            Operator::List(_) => Outcome::unsupported("list", NESTED_LIST),
            _ => evaluate(child, input),
        };
        match outcome.truth {
            Truth::No => return Outcome::definite(Truth::No),
            Truth::Unknown => {
                undecided = true;
                gaps.extend(outcome.gaps);
            }
            Truth::Yes => {}
        }
        for warning in outcome.warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    Outcome {
        truth: if undecided {
            Truth::Unknown
        } else {
            Truth::Yes
        },
        gaps,
        warnings,
    }
}

/// `Operator.Match` for one leaf, dispatching on the operand in the daemon's
/// order. An operand `Match` has no branch for falls through to `false`.
fn evaluate_leaf(leaf: &Leaf, input: &SimulationInput) -> Outcome {
    let operand = leaf.operand.as_str();
    if leaf.kind == Kind::Lists || operand.starts_with("lists.") {
        return Outcome::unsupported(operand, LISTS);
    }
    match operand {
        "user.name" => Outcome::unsupported(operand, USER_NAME),
        "process.parent.path" => parent_path(leaf, input),
        "process.hash.md5" | "process.hash.sha1" => hash(leaf, input),
        "dest.network" => network_operand(leaf, input.dest_ip.as_deref(), DEST_IP, BAD_DEST_IP),
        "source.network" => network_operand(leaf, input.src_ip.as_deref(), SRC_IP, BAD_SRC_IP),
        _ => match (text_subject(operand, input), leaf.kind) {
            (Subject::NotAnOperand, _) => Outcome::definite(Truth::No),
            (_, Kind::Network | Kind::Lists) => Outcome::unsupported(operand, BAD_PAIRING),
            (Subject::Missing(what), _) => Outcome::unevaluated(operand, what),
            (Subject::Known(subject), _) => text_outcome(leaf, &subject),
        },
    }
}

/// The comparison the operator's type makes, or the pairing failure for a
/// type that can't compare text.
fn text_outcome(leaf: &Leaf, subject: &str) -> Outcome {
    match text_cmp(leaf, subject) {
        Some(matched) => Outcome::from_bool(matched),
        None => Outcome::unsupported(&leaf.operand, BAD_PAIRING),
    }
}

fn text_cmp(leaf: &Leaf, subject: &str) -> Option<bool> {
    match leaf.kind {
        Kind::Simple => Some(compare::simple_cmp(subject, &leaf.data, leaf.sensitive)),
        Kind::Regexp => Some(compare::regexp_cmp(subject, &leaf.data, leaf.sensitive)),
        Kind::Network | Kind::Lists => None,
    }
}

enum Subject {
    Known(String),
    /// An input the caller left unknown.
    Missing(&'static str),
    NotAnOperand,
}

fn known_or<T: ToString>(value: Option<T>, missing: &'static str) -> Subject {
    value.map_or(Subject::Missing(missing), |v| Subject::Known(v.to_string()))
}

/// The string `Match` hands a leaf's callback, for every operand that passes a
/// string.
fn text_subject(operand: &str, input: &SimulationInput) -> Subject {
    match operand {
        "process.path" => known_or(input.process_path.as_ref(), PROCESS_PATH),
        "process.command" => known_or(input.command.as_ref(), COMMAND),
        "process.id" => known_or(input.pid, PID),
        "dest.host" => Subject::Known(input.dest_host.clone()),
        "dest.ip" => ip_subject(input.dest_ip.as_deref(), DEST_IP, BAD_DEST_IP),
        "dest.port" => Subject::Known(input.dest_port.to_string()),
        "user.id" => known_or(input.uid, UID),
        "source.ip" => ip_subject(input.src_ip.as_deref(), SRC_IP, BAD_SRC_IP),
        "source.port" => known_or(input.src_port, SRC_PORT),
        "protocol" => Subject::Known(input.protocol.clone()),
        "iface.in" => known_or(input.iface_in.as_ref(), IFACE_IN),
        "iface.out" => known_or(input.iface_out.as_ref(), IFACE_OUT),
        _ => match operand.strip_prefix("process.env.") {
            // An unset variable compares as "" — but only once the
            // environment is known.
            Some(name) => match &input.env {
                Some(env) => Subject::Known(
                    env.get(name.trim_matches(is_env_trim))
                        .cloned()
                        .unwrap_or_default(),
                ),
                None => Subject::Missing(ENV),
            },
            None => Subject::NotAnOperand,
        },
    }
}

/// `net.IP.String()` of the typed address.
fn ip_subject(text: Option<&str>, missing: &'static str, invalid: &'static str) -> Subject {
    match text {
        None => Subject::Missing(missing),
        Some(text) => match network::parse_ip(text) {
            Some(ip) => Subject::Known(ip.to_string()),
            None => Subject::Missing(invalid),
        },
    }
}

/// `process.parent.path` is true when **any** ancestor's path matches.
fn parent_path(leaf: &Leaf, input: &SimulationInput) -> Outcome {
    let Some(ancestors) = &input.parent_paths else {
        return Outcome::unevaluated(&leaf.operand, PARENT_PATHS);
    };
    let mut matched = false;
    for path in ancestors {
        match text_cmp(leaf, path) {
            Some(true) => {
                matched = true;
                break;
            }
            Some(false) => {}
            None => return Outcome::unsupported(&leaf.operand, BAD_PAIRING),
        }
    }
    Outcome::from_bool(matched)
}

/// The `process.hash.*` branch of `Match`: `ret` starts `true`; with
/// checksums off it stays `true`; with them on, it is overwritten while
/// iterating the process's checksums — **every** algorithm's value, whatever
/// the operand's — so a process with none still matches.
fn hash(leaf: &Leaf, input: &SimulationInput) -> Outcome {
    match input.checksums_enabled {
        Some(false) => return Outcome::yes_with(HASH_OFF),
        None => return Outcome::yes_with(HASH_UNKNOWN),
        Some(true) => {}
    }
    let Some(checksums) = &input.checksums else {
        return Outcome::unevaluated(&leaf.operand, CHECKSUM);
    };
    if checksums.is_empty() {
        return Outcome::yes_with(HASH_NONE_RECORDED);
    }
    for value in checksums.values() {
        let matched = match leaf.kind {
            // `hashCmp`: exact, and an empty hash is a fake match.
            Kind::Simple if value.is_empty() => return Outcome::yes_with(HASH_NONE_RECORDED),
            Kind::Simple => compare::hash_cmp(value, &leaf.data),
            Kind::Regexp => compare::regexp_cmp(value, &leaf.data, leaf.sensitive),
            Kind::Network | Kind::Lists => return Outcome::unsupported(&leaf.operand, BAD_PAIRING),
        };
        if matched {
            return Outcome::definite(Truth::Yes);
        }
    }
    Outcome::definite(Truth::No)
}

/// `dest.network` / `source.network`: `Match` passes a `net.IP`, which only
/// the `network` type can compare. `simple` would panic in the daemon; `reCmp`
/// sees a non-string and returns false.
fn network_operand(
    leaf: &Leaf,
    ip: Option<&str>,
    missing: &'static str,
    invalid: &'static str,
) -> Outcome {
    match leaf.kind {
        Kind::Network => {}
        Kind::Regexp => return Outcome::definite(Truth::No),
        Kind::Simple | Kind::Lists => return Outcome::unsupported(&leaf.operand, BAD_PAIRING),
    }
    let Some(text) = ip else {
        return Outcome::unevaluated(&leaf.operand, missing);
    };
    let Some(ip) = network::parse_ip(text) else {
        return Outcome::unevaluated(&leaf.operand, invalid);
    };
    match network::contains(&leaf.data, ip) {
        Some(inside) => Outcome::from_bool(inside),
        None => Outcome::unsupported(&leaf.operand, UNKNOWN_NETWORK),
    }
}
