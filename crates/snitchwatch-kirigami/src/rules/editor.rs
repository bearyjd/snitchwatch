//! The rule editor's draft (roadmap P2.1), without Qt.
//!
//! A [`RuleDraft`] is what `RuleEditorSheet.qml` edits: the rule's fields
//! and a list of ANDed conditions. One condition is a leaf; two or more are
//! a `list` with operand `list`, the shape #50 builds. The builder offers
//! each operand only the match kinds `validate_operator` accepts for it, so
//! it can't express a pairing the bridge would refuse.
//!
//! The bridge is authoritative. [`check`] runs the very function the bridge
//! runs (`rule_policy::check_wire_rule` with the `Editor` profile) for
//! instant feedback, and the import preview's cautions
//! (`rule_io::edit_cautions`) against the rule being replaced. Every string
//! is plain text.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use snitchwatch_bridge::rule_policy::{binds_to_programs, check_wire_rule, PolicyProfile};
use snitchwatch_bridge::translator::process_binding::is_bindable_process_path;
use snitchwatch_proto::protocol::Operator;

use super::simulator::SimulationForm;

/// How a condition compares its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchKind {
    /// `simple`: equal (ignoring case unless case-sensitive).
    Exact,
    /// `regexp`.
    Pattern,
    /// `network`: a CIDR.
    Network,
}

impl MatchKind {
    fn wire_type(self) -> &'static str {
        match self {
            Self::Exact => "simple",
            Self::Pattern => "regexp",
            Self::Network => "network",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub operand: String,
    pub kind: MatchKind,
    pub value: String,
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDraft {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    /// `allow`, `deny` or `reject`.
    pub action: String,
    /// `always`, `until restart`, or a time such as `5m`.
    pub duration: String,
    pub precedence: bool,
    pub nolog: bool,
    pub conditions: Vec<Condition>,
}

/// An operand the builder offers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperandChoice {
    pub operand: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub kinds: Vec<MatchKind>,
    /// What the daemon compares, in plain words.
    pub help: &'static str,
}

const TEXT: &[MatchKind] = &[MatchKind::Exact, MatchKind::Pattern];
const EXACT: &[MatchKind] = &[MatchKind::Exact];
const NETWORK: &[MatchKind] = &[MatchKind::Network];

/// `(operand, group, label, kinds, help)`, in the builder's order.
type OperandRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static [MatchKind],
    &'static str,
);

const OPERANDS: [OperandRow; 15] = [
    (
        "process.path",
        "Process",
        "Program path",
        TEXT,
        "The program's full path, such as /usr/bin/curl.",
    ),
    (
        "process.parent.path",
        "Process",
        "Parent program path",
        TEXT,
        "Matches if any parent process, up to the first one, has this path.",
    ),
    (
        "process.command",
        "Process",
        "Command line",
        TEXT,
        "The program's command line: its arguments joined with spaces.",
    ),
    (
        "user.id",
        "Process",
        "User ID",
        TEXT,
        "The user the program runs as, by number.",
    ),
    (
        "user.name",
        "Process",
        "User name",
        EXACT,
        "The user the program runs as, by name.",
    ),
    (
        "dest.host",
        "Destination",
        "Host name",
        TEXT,
        "The host name the program connects to.",
    ),
    (
        "dest.ip",
        "Destination",
        "IP address",
        TEXT,
        "The address the program connects to.",
    ),
    (
        "dest.network",
        "Destination",
        "Network",
        NETWORK,
        "A network, such as 192.168.1.0/24.",
    ),
    (
        "dest.port",
        "Destination",
        "Port",
        TEXT,
        "The port the program connects to.",
    ),
    (
        "source.ip",
        "Source",
        "IP address",
        TEXT,
        "The local address.",
    ),
    ("source.port", "Source", "Port", TEXT, "The local port."),
    (
        "source.network",
        "Source",
        "Network",
        NETWORK,
        "A local network.",
    ),
    (
        "protocol",
        "Network",
        "Protocol",
        TEXT,
        "Such as tcp or udp.",
    ),
    (
        "iface.in",
        "Network",
        "Incoming interface",
        TEXT,
        "Such as eth0.",
    ),
    (
        "iface.out",
        "Network",
        "Outgoing interface",
        TEXT,
        "Such as wlan0.",
    ),
];

/// The operands the builder offers, grouped. Never `true`, `list`, hashes
/// (they match every program while checksums are off) or `process.id` (it
/// names whatever process gets that number next).
pub fn operands() -> Vec<OperandChoice> {
    OPERANDS
        .iter()
        .map(|&(operand, group, label, kinds, help)| OperandChoice {
            operand,
            group,
            label,
            kinds: kinds.to_vec(),
            help,
        })
        .collect()
}

fn offered(operand: &str, kind: MatchKind) -> bool {
    OPERANDS
        .iter()
        .any(|&(name, _, _, kinds, _)| name == operand && kinds.contains(&kind))
}

/// Duration presets: (wire value, label). `once` isn't offered.
pub const DURATION_PRESETS: [(&str, &str); 6] = [
    ("5m", "5 minutes"),
    ("1h", "1 hour"),
    ("12h", "12 hours"),
    ("24h", "1 day"),
    ("until restart", "Until the firewall restarts"),
    ("always", "Forever"),
];

impl Condition {
    fn to_wire(&self) -> Value {
        json!({
            "type": self.kind.wire_type(),
            "operand": self.operand,
            "data": self.value,
            "sensitive": self.case_sensitive,
        })
    }

    fn to_operator(&self) -> Operator {
        Operator {
            r#type: self.kind.wire_type().into(),
            operand: self.operand.clone(),
            data: self.value.clone(),
            sensitive: self.case_sensitive,
            ..Default::default()
        }
    }

    fn from_operator(op: &Operator) -> Result<Self, String> {
        let kind = match op.r#type.as_str() {
            "simple" => MatchKind::Exact,
            "regexp" => MatchKind::Pattern,
            "network" => MatchKind::Network,
            _ => return Err(NOT_OFFERED.to_string()),
        };
        if !offered(&op.operand, kind) {
            return Err(NOT_OFFERED.to_string());
        }
        Ok(Self {
            operand: op.operand.clone(),
            kind,
            value: op.data.clone(),
            case_sensitive: op.sensitive,
        })
    }
}

const NOT_OFFERED: &str = "it uses a condition the editor doesn't offer";

impl RuleDraft {
    /// The rule in #48's wire shape.
    pub fn to_wire(&self) -> Value {
        let operator = match self.conditions.as_slice() {
            [one] => one.to_wire(),
            many => json!({
                "type": "list",
                "operand": "list",
                "operands": many.iter().map(Condition::to_wire).collect::<Vec<_>>(),
            }),
        };
        json!({
            "name": self.name,
            "enabled": self.enabled,
            "action": self.action,
            "duration": self.duration,
            "description": self.description,
            "precedence": self.precedence,
            "nolog": self.nolog,
            "operator": operator,
        })
    }

    /// A draft of an existing rule, or why the editor can't change it: the
    /// bridge's `Editor` profile refuses it, or it uses a condition the
    /// builder doesn't offer. Such rules keep toggle and delete.
    pub fn from_wire(rule: &Value) -> Result<Self, String> {
        let parsed = check_wire_rule(rule, PolicyProfile::Editor)
            .map_err(|problems| plain_problems(&problems).join(" "))?;
        let op = parsed.operator.as_ref().ok_or(NOT_OFFERED)?;
        let leaves: Vec<&Operator> = if op.r#type == "list" {
            op.list.iter().collect()
        } else {
            vec![op]
        };
        let conditions = leaves
            .into_iter()
            .map(Condition::from_operator)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            name: parsed.name,
            description: parsed.description,
            enabled: parsed.enabled,
            action: parsed.action,
            duration: parsed.duration,
            precedence: parsed.precedence,
            nolog: parsed.nolog,
            conditions,
        })
    }
}

/// A blank draft: a deny that lasts forever, with no conditions yet.
pub fn new_draft() -> RuleDraft {
    RuleDraft {
        name: String::new(),
        description: String::new(),
        enabled: true,
        action: "deny".into(),
        duration: "always".into(),
        precedence: false,
        nolog: false,
        conditions: Vec::new(),
    }
}

/// A draft for a connection (the simulator's prefill form): its program
/// (only a real program file's path), then its host or, without one, its
/// address, then its port.
pub fn prefill(form: &SimulationForm) -> RuleDraft {
    let exact = |operand: &str, value: &str, case_sensitive| Condition {
        operand: operand.into(),
        kind: MatchKind::Exact,
        value: value.into(),
        case_sensitive,
    };
    let mut conditions = Vec::new();
    if is_bindable_process_path(&form.process_path) {
        conditions.push(exact("process.path", &form.process_path, true));
    }
    if !form.dest_host_empty && !form.dest_host.is_empty() {
        conditions.push(exact("dest.host", &form.dest_host, false));
    } else if !form.dest_ip.is_empty() {
        conditions.push(exact("dest.ip", &form.dest_ip, false));
    }
    if (1..=65535).contains(&form.dest_port) {
        conditions.push(exact("dest.port", &form.dest_port.to_string(), false));
    }
    let draft = RuleDraft {
        conditions,
        ..new_draft()
    };
    RuleDraft {
        name: suggest_name(&draft),
        ..draft
    }
}

/// `snitchwatch-<action>-<target>-<8 hex>`, like `rule_name_for`; always a
/// valid, unreserved rule name.
pub fn suggest_name(draft: &RuleDraft) -> String {
    use std::hash::{Hash, Hasher};
    let target = draft
        .conditions
        .iter()
        .find(|c| matches!(c.operand.as_str(), "dest.host" | "dest.ip"))
        .or_else(|| {
            draft
                .conditions
                .iter()
                .find(|c| c.operand == "process.path")
        })
        .map(|c| c.value.rsplit('/').next().unwrap_or("").to_string())
        .unwrap_or_default();
    let target: String = target
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .take(60)
        .collect::<String>()
        .to_ascii_lowercase();
    let target = target.trim_matches('.');
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    draft.to_wire().to_string().hash(&mut hasher);
    let action = match draft.action.as_str() {
        "allow" | "reject" => draft.action.as_str(),
        _ => "deny",
    };
    let hash = format!("{:08x}", hasher.finish() as u32);
    if target.is_empty() {
        format!("snitchwatch-{action}-rule-{hash}")
    } else {
        format!("snitchwatch-{action}-{target}-{hash}")
    }
}

/// What the sheet shows for a draft.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorCheck {
    /// Why the bridge would refuse it; empty when it would be sent.
    pub problems: Vec<String>,
    /// What the rule does that may surprise.
    pub warnings: Vec<String>,
    /// What replacing `old` loosens; saving them takes a second click.
    pub cautions: Vec<String>,
}

/// Policy problems as plain sentences, each with where it is in plain words.
pub(crate) fn plain_problems(
    problems: &[snitchwatch_bridge::rule_policy::RuleProblem],
) -> Vec<String> {
    problems
        .iter()
        .map(|p| match super::io_view::plain_location(&p.path) {
            Some(place) => format!("{} ({place})", p.reason),
            None => p.reason.clone(),
        })
        .collect()
}

/// Check a draft as the bridge will, against `old` (the cached rule it
/// replaces, in the wire shape) when editing.
pub fn check(draft: &RuleDraft, old: Option<&Value>) -> EditorCheck {
    let mut result = EditorCheck {
        warnings: warnings(draft),
        ..Default::default()
    };
    match check_wire_rule(&draft.to_wire(), PolicyProfile::Editor) {
        Err(problems) => result.problems = plain_problems(&problems),
        Ok(rule) => {
            let old = old.and_then(|old| check_wire_rule(old, PolicyProfile::Editor).ok());
            result.cautions = snitchwatch_bridge::rule_io::edit_cautions(old.as_ref(), &rule);
        }
    }
    result
}

fn unanchored_path_pattern(condition: &Condition) -> bool {
    matches!(
        condition.operand.as_str(),
        "process.path" | "process.parent.path"
    ) && condition.kind == MatchKind::Pattern
        && !(condition.value.starts_with('^') && condition.value.ends_with('$'))
}

fn warnings(draft: &RuleDraft) -> Vec<String> {
    let mut out = Vec::new();
    // From the draft itself: a problem elsewhere doesn't unbind a program.
    let binds = draft
        .conditions
        .iter()
        .any(|c| binds_to_programs(&c.to_operator()));
    if !binds {
        out.push("Applies to every program: no condition names one program file.".into());
    }
    if draft
        .conditions
        .iter()
        .any(|c| c.operand == "process.path" && c.kind == MatchKind::Pattern)
    {
        out.push(
            "A program path pattern can match more programs than you mean: every program \
             whose path fits it. ^/usr/bin/ matches every program in /usr/bin, including \
             shells and interpreters that run other programs."
                .into(),
        );
    }
    if draft.conditions.iter().any(unanchored_path_pattern) {
        out.push(
            "A program path pattern without ^ at the start and $ at the end also matches \
             longer paths that contain it: /usr/bin/curl matches /home/me/usr/bin/curl too."
                .into(),
        );
    }
    if draft.precedence {
        out.push("Decides before other rules, including ones that block.".into());
    }
    if draft.nolog {
        out.push("Hides this rule's connections from Snitchwatch's lists and counts.".into());
    }
    if draft.duration != "always" {
        out.push("Lost when the firewall restarts.".into());
    }
    if !matches!(draft.duration.as_str(), "always" | "until restart") {
        out.push(
            "Removed automatically when its time runs out. An existing rule that keeps the \
             same time is removed when its original time runs out."
                .into(),
        );
    }
    out
}
