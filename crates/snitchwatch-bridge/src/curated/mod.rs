//! Curated default rules for background services (prompt-slot plan Part D;
//! owner decision S3: opt-in, `/usr` paths only, host-constrained, no
//! per-user regexps in v1).
//!
//! The list is data, `data/curated-defaults-v1.json`, reviewed and built
//! into the bridge. Each entry lets one program, named by its exact
//! absolute path under `/usr`, reach the narrowest destination the
//! bazzite-tower capture supports: one host or this computer only (never
//! any address), one port, one protocol over both IP versions. Each says
//! in plain text why it is there.
//!
//! Nothing is on by default. The user turns entries on (`store`), and
//! `reconcile` installs them under the reserved `snitchwatch-default-`
//! prefix, as `allow`, `always`, never `precedence` (a precedence allow
//! would win over a blocklist's deny).
//!
//! [`check_curated_rule`] is the curated-specific allowlist: a rule is sent
//! under the prefix only in exactly the shape [`CuratedEntry::rule`] builds,
//! and only if it also passes the rule editor's policy checks.

pub mod manager;
pub mod reconcile;
pub mod store;
pub mod wire;

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use snitchwatch_proto::protocol::{Operator, Rule};

use crate::rule_name::CURATED_DEFAULT_RULE_NAME_PREFIX;
use crate::rule_policy::{validate_user_rule, PolicyProfile};
use crate::translator::process_binding::is_bindable_process_path;

const DATA: &str = include_str!("../../data/curated-defaults-v1.json");

/// The `description` of every curated rule.
pub const DESCRIPTION: &str = "snitchwatch curated default v1";

/// `dest.ip` for an entry limited to this computer: IPv4 or IPv6 loopback.
const LOOPBACK_PATTERN: &str = r"^(127\.0\.0\.1|::1)$";

/// The transport an entry allows, over IPv4 and IPv6 (the daemon names the
/// IPv6 forms `tcp6`/`udp6`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    fn pattern(self) -> &'static str {
        match self {
            Self::Tcp => "^tcp6?$",
            Self::Udp => "^udp6?$",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
        }
    }
}

/// One reviewed entry of the data file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CuratedEntry {
    pub id: String,
    /// The program's exact absolute path, under `/usr`.
    pub path: String,
    /// The one host it may reach. Exactly one of `host` and `loopback`.
    #[serde(default)]
    pub host: Option<String>,
    /// Only this computer (127.0.0.1 and ::1).
    #[serde(default)]
    pub loopback: bool,
    pub port: u16,
    pub protocol: Protocol,
    /// Why it is offered, in plain text.
    pub why: String,
    /// Where the capture shows it.
    pub evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataFile {
    version: u32,
    entries: Vec<CuratedEntry>,
}

/// The entries offered. An invalid data file is a build mistake the tests
/// catch; at run time it offers nothing rather than something unchecked.
pub fn entries() -> &'static [CuratedEntry] {
    static ENTRIES: OnceLock<Vec<CuratedEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        parse(DATA).unwrap_or_else(|error| {
            tracing::error!(%error, "the curated defaults list is invalid; none are offered");
            Vec::new()
        })
    })
}

/// Whether a GUI may turn the daemon's `rule` on or off: an entry's rule
/// exactly as the data file builds it, apart from `enabled`. A copy edited
/// outside Snitchwatch, or a rule squatting on the prefix, is left alone.
pub fn toggleable(rule: &Rule) -> bool {
    entry_for_rule_name(&rule.name)
        .is_some_and(|entry| reconcile::same_ignoring_enabled(&entry.rule(), rule))
}

/// The `enabled` a GUI's wire rule asks for, if it is `current` apart from
/// `enabled` (same name, same everything else); `None` otherwise.
pub fn requested_toggle(current: &Rule, wire: &serde_json::Value) -> Option<bool> {
    let wanted = crate::rule_wire::rule_from_wire(wire).ok()?;
    let pure =
        wanted.name == current.name && crate::rule_io::only_enabled_differs(current, &wanted);
    pure.then_some(wanted.enabled)
}

/// The entry whose rule is called `name`.
pub fn entry_for_rule_name(name: &str) -> Option<&'static CuratedEntry> {
    entries().iter().find(|entry| entry.rule_name() == name)
}

fn parse(raw: &str) -> Result<Vec<CuratedEntry>, String> {
    let file: DataFile =
        serde_json::from_str(raw).map_err(|_| "the data file isn't valid".to_string())?;
    if file.version != 1 {
        return Err("unknown data file version".into());
    }
    for (index, entry) in file.entries.iter().enumerate() {
        entry.check()?;
        if file.entries[..index]
            .iter()
            .any(|other| other.id == entry.id)
        {
            return Err("two entries share an id".into());
        }
    }
    Ok(file.entries)
}

impl CuratedEntry {
    pub fn rule_name(&self) -> String {
        format!("{CURATED_DEFAULT_RULE_NAME_PREFIX}{}", self.id)
    }

    /// The daemon rule for this entry.
    pub fn rule(&self) -> Rule {
        let mut leaves = vec![leaf("simple", "process.path", &self.path, true)];
        if let Some(host) = &self.host {
            leaves.push(leaf("simple", "dest.host", host, false));
        }
        if self.loopback {
            leaves.push(leaf("regexp", "dest.ip", LOOPBACK_PATTERN, false));
        }
        leaves.push(leaf("simple", "dest.port", &self.port.to_string(), false));
        leaves.push(leaf("regexp", "protocol", self.protocol.pattern(), false));
        Rule {
            created: 0,
            name: self.rule_name(),
            description: DESCRIPTION.to_string(),
            enabled: true,
            precedence: false,
            nolog: false,
            action: "allow".to_string(),
            duration: "always".to_string(),
            operator: Some(Operator {
                r#type: "list".to_string(),
                operand: "list".to_string(),
                data: String::new(),
                sensitive: false,
                list: leaves,
            }),
        }
    }

    /// Exactly what the rule allows, in plain text.
    pub fn allows(&self) -> String {
        let to = match &self.host {
            Some(host) => host.clone(),
            None => "this computer only (127.0.0.1 and ::1)".to_string(),
        };
        format!(
            "{} may connect to {to} on {} port {}, over IPv4 and IPv6.",
            self.path,
            self.protocol.label(),
            self.port
        )
    }

    fn check(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err("an entry id isn't lowercase letters, digits and dashes".into());
        }
        if !usr_program(&self.path) {
            return Err("an entry's program isn't an exact absolute path under /usr".into());
        }
        if self.host.as_deref().is_some_and(|host| !valid_host(host)) {
            return Err("an entry's host isn't one plain host name".into());
        }
        if self.host.is_some() == self.loopback {
            return Err("an entry names neither or both of a host and this computer".into());
        }
        if self.port == 0 {
            return Err("an entry has no port".into());
        }
        if !plain_text(&self.why) || !plain_text(&self.evidence) {
            return Err("an entry's text isn't short plain text".into());
        }
        check_curated_rule(&self.rule())
    }
}

fn leaf(r#type: &str, operand: &str, data: &str, sensitive: bool) -> Operator {
    Operator {
        r#type: r#type.to_string(),
        operand: operand.to_string(),
        data: data.to_string(),
        sensitive,
        list: Vec::new(),
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    (1..=48).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// An exact absolute program path under `/usr` (#44's bindable-path rule).
fn usr_program(path: &str) -> bool {
    path.starts_with("/usr/") && is_bindable_process_path(path)
}

/// One plain host name: lowercase labels of letters, digits and dashes,
/// no wildcard and no IP literal.
fn valid_host(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.parse::<std::net::IpAddr>().is_err()
        && host.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

fn plain_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= 400
        && !text
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '&'))
}

/// The curated-specific allowlist. A rule under the reserved prefix is sent
/// only if it is an `always`, non-precedence `allow` with our description,
/// whose conditions are exactly: an exact `/usr` program path (case
/// sensitive); one plain host or this computer; one port; one transport. It must also pass the rule editor's policy checks.
pub fn check_curated_rule(rule: &Rule) -> Result<(), String> {
    let id = rule
        .name
        .strip_prefix(CURATED_DEFAULT_RULE_NAME_PREFIX)
        .filter(|id| valid_id(id))
        .ok_or("the name isn't a curated default's")?;
    if rule.action != "allow" || rule.duration != "always" || rule.precedence {
        return Err("a curated default is an always, non-precedence allow".into());
    }
    if rule.description != DESCRIPTION {
        return Err("the description isn't a curated default's".into());
    }
    let op = rule.operator.as_ref().ok_or("the rule has no conditions")?;
    // The wire shape reports a list's operand empty.
    if op.r#type != "list" || !(op.operand.is_empty() || op.operand == "list") {
        return Err("a curated default's conditions are one list".into());
    }
    check_leaves(&op.list)?;
    // The editor's checks, on a copy with a name the editor would take.
    let renamed = Rule {
        name: format!("curated-check-{id}"),
        ..rule.clone()
    };
    validate_user_rule(&renamed, PolicyProfile::Editor)
        .map_err(|_| "the rule fails the rule editor's checks".to_string())
}

fn check_leaves(leaves: &[Operator]) -> Result<(), String> {
    let mut rest = leaves.iter().peekable();
    match rest.next() {
        Some(op)
            if op.r#type == "simple"
                && op.operand == "process.path"
                && op.sensitive
                && usr_program(&op.data) => {}
        _ => return Err("the first condition isn't an exact /usr program path".into()),
    }
    match rest.next() {
        Some(op)
            if op.operand == "dest.host"
                && op.r#type == "simple"
                && !op.sensitive
                && valid_host(&op.data) => {}
        Some(op)
            if op.operand == "dest.ip" && op.r#type == "regexp" && op.data == LOOPBACK_PATTERN => {}
        _ => return Err("the destination isn't one plain host name or this computer".into()),
    }
    match rest.next() {
        Some(op)
            if op.r#type == "simple"
                && op.operand == "dest.port"
                && op.data.parse::<u16>().is_ok_and(|port| port != 0)
                && !op.data.starts_with('0') => {}
        _ => return Err("the port condition isn't one port".into()),
    }
    match rest.next() {
        Some(op)
            if op.r#type == "regexp"
                && op.operand == "protocol"
                && [Protocol::Tcp, Protocol::Udp]
                    .iter()
                    .any(|protocol| op.data == protocol.pattern()) => {}
        _ => return Err("the protocol condition isn't TCP or UDP".into()),
    }
    if rest.next().is_some() || leaves.iter().any(|op| !op.list.is_empty()) {
        return Err("a curated default has other conditions".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
