//! Curated default rules for background services (prompt-slot plan Part D;
//! owner decision S3: opt-in, `/usr` paths only, host-constrained, no
//! per-user regexps in v1).
//!
//! The list is data, `data/curated-defaults-v1.json`, reviewed and built
//! into the bridge. Each entry lets one program, named by its exact
//! absolute path under `/usr`, reach the narrowest destination the
//! bazzite-tower capture supports: one host or this computer only, one
//! port, one protocol over both IP versions. The one exception is the
//! system resolver's DNS (owner decision S6): its upstream server changes
//! with every network, so it is offered for any address, port 53, TCP and
//! UDP, and no other program can be. A path alone doesn't name a sender
//! (any user can run that binary with `LD_PRELOAD`), so that entry is also
//! pinned to the resolver's own account by user ID. Each entry says in plain
//! text why it is there.
//!
//! Nothing is on by default. The user turns entries on (`store`), and
//! `reconcile` installs them under the reserved `snitchwatch-default-`
//! prefix, as `allow`, `always`, never `precedence` (a precedence allow
//! would win over a blocklist's deny).
//!
//! [`check_curated_rule`] is the curated-specific allowlist: a rule is sent
//! under the prefix only in exactly the shape [`CuratedEntry::rule`] builds,
//! and only if it also passes the rule editor's policy checks.

pub mod canonical;
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

/// The one program that may be offered for any address (owner decision S6,
/// issue #117), and the one port, transport and sender it gets there: the
/// system resolver's DNS, whose upstream server differs on every network,
/// run by its own account. The pin is the user ID, not `user.name`: the
/// daemon's `Compile` rewrites a `user.name` leaf's data to the uid and
/// saves the rule that way, so after a restart the file's number would be
/// looked up as a name and the rule would fail to load. Fedora's
/// `sysusers.d/systemd-resolve.conf` fixes `systemd-resolve` at 193; a rule
/// pinned to the wrong ID matches nothing (fail closed).
const DNS_PROGRAM: &str = "/usr/lib/systemd/systemd-resolved";
const DNS_PORT: u16 = 53;
const DNS_USER_ID: &str = "193";

/// The transport an entry allows, over IPv4 and IPv6 (the daemon names the
/// IPv6 forms `tcp6`/`udp6`). `TcpAndUdp` is the DNS entry's alone
/// ([`check_leaves`]): one anchored regexp for both, as the daemon's
/// `protocol` operand is compared to one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
    #[serde(rename = "tcp+udp")]
    TcpAndUdp,
}

impl Protocol {
    fn pattern(self) -> &'static str {
        match self {
            Self::Tcp => "^tcp6?$",
            Self::Udp => "^udp6?$",
            Self::TcpAndUdp => "^(tcp|udp)6?$",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Udp => "UDP",
            Self::TcpAndUdp => "TCP and UDP",
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
    /// The one host it may reach. Exactly one of `host`, `loopback` and
    /// `anyAddress`.
    #[serde(default)]
    pub host: Option<String>,
    /// Only this computer (127.0.0.1 and ::1).
    #[serde(default)]
    pub loopback: bool,
    /// Any address: no destination condition. Only the system resolver's
    /// DNS ([`DNS_PROGRAM`], run by [`DNS_USER_ID`], port 53, TCP and UDP)
    /// passes [`check_leaves`].
    #[serde(default, rename = "anyAddress")]
    pub any_address: bool,
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
/// Reconcile then treats every recorded copy as retired: an unedited one is
/// deleted, an edited one left alone (the safe direction: no allow stays
/// that the bridge can no longer describe).
pub fn entries() -> &'static [CuratedEntry] {
    static ENTRIES: OnceLock<Vec<CuratedEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        parse(DATA).unwrap_or_else(|error| {
            tracing::error!(%error, "the curated defaults list is invalid; none are offered");
            Vec::new()
        })
    })
}

/// Whether a GUI may turn the daemon's `rule` on or off: an entry's rule,
/// unedited ([`canonical::is_unedited`]). A copy edited outside
/// Snitchwatch, or a rule squatting on the prefix, is left alone.
pub fn toggleable(rule: &Rule) -> bool {
    entry_for_rule_name(&rule.name)
        .is_some_and(|entry| canonical::is_unedited(Some(entry), None, rule))
}

/// The `enabled` a GUI's wire rule asks for, if it is `current` apart from
/// `enabled` (same name, same everything else); `None` otherwise.
pub fn requested_toggle(current: &Rule, wire: &serde_json::Value) -> Option<bool> {
    let wanted = crate::rule_wire::rule_from_wire(wire).ok()?;
    (canonical::canonical(&wanted) == canonical::canonical(current)).then_some(wanted.enabled)
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
        // `any_address` adds no destination condition: that is what it
        // means. It pins the sender instead.
        if self.any_address {
            leaves.push(leaf("simple", "user.id", DNS_USER_ID, false));
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
        let to = match (&self.host, self.any_address) {
            (Some(host), _) => host.clone(),
            (None, true) => "any address".to_string(),
            (None, false) => "this computer only (127.0.0.1 and ::1)".to_string(),
        };
        let sender = if self.any_address {
            format!(
                ", but only while it runs as user ID {DNS_USER_ID} (the systemd-resolve account)"
            )
        } else {
            String::new()
        };
        format!(
            "{} may connect to {to} on {} port {}, over IPv4 and IPv6{sender}.",
            self.path,
            self.protocol.label(),
            self.port
        )
    }

    /// Whether the entry allows more than one named place: its GUI never
    /// turns it on in bulk ("Turn all on"), only by itself.
    pub fn broad(&self) -> bool {
        self.any_address
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
        let destinations = usize::from(self.host.is_some())
            + usize::from(self.loopback)
            + usize::from(self.any_address);
        if destinations != 1 {
            return Err(
                "an entry names none or several of a host, this computer and any address".into(),
            );
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

/// An exact absolute program path under `/usr`, not `/usr/local` (#44's
/// bindable-path rule): a program the system image ships.
fn usr_program(path: &str) -> bool {
    path.starts_with("/usr/") && !path.starts_with("/usr/local/") && is_bindable_process_path(path)
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
/// sensitive); one plain host or this computer; one port; one transport.
/// The one exception is the system resolver's DNS (owner decision S6): no
/// destination, but its account's user ID pinned right after the path, port
/// 53, TCP and UDP. A rule with no destination is refused for any other
/// program, and without that exact pin. It must also pass the rule editor's
/// policy checks.
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

type Leaves<'a> = std::iter::Peekable<std::slice::Iter<'a, Operator>>;

fn check_leaves(leaves: &[Operator]) -> Result<(), String> {
    let mut rest = leaves.iter().peekable();
    let program = check_program(rest.next())?;
    let any_address = check_destination(&mut rest, program)?;
    check_port(rest.next(), any_address)?;
    check_protocol(rest.next(), any_address)?;
    if rest.next().is_some() || leaves.iter().any(|op| !op.list.is_empty()) {
        return Err("a curated default has other conditions".into());
    }
    Ok(())
}

fn check_program(leaf: Option<&Operator>) -> Result<&str, String> {
    match leaf {
        Some(op)
            if op.r#type == "simple"
                && op.operand == "process.path"
                && op.sensitive
                && usr_program(&op.data) =>
        {
            Ok(op.data.as_str())
        }
        _ => Err("the first condition isn't an exact /usr program path".into()),
    }
}

/// The destination: one plain host or this computer, or no destination
/// condition at all, which is any address. Returns whether it is any
/// address; that is the system resolver's alone, and it must name its
/// account, so a rule without the pin is refused.
fn check_destination(rest: &mut Leaves<'_>, program: &str) -> Result<bool, String> {
    let named = rest
        .peek()
        .is_some_and(|op| matches!(op.operand.as_str(), "dest.host" | "dest.ip"));
    if named {
        return match rest.next() {
            Some(op)
                if op.operand == "dest.host"
                    && op.r#type == "simple"
                    && !op.sensitive
                    && valid_host(&op.data) =>
            {
                Ok(false)
            }
            Some(op)
                if op.operand == "dest.ip"
                    && op.r#type == "regexp"
                    && op.data == LOOPBACK_PATTERN =>
            {
                Ok(false)
            }
            _ => Err("the destination isn't one plain host name or this computer".into()),
        };
    }
    if program != DNS_PROGRAM {
        return Err("only the system resolver's DNS may reach any address".into());
    }
    match rest.next() {
        Some(op)
            if op.r#type == "simple"
                && op.operand == "user.id"
                && !op.sensitive
                && op.data == DNS_USER_ID =>
        {
            Ok(true)
        }
        _ => Err("any address needs the system resolver's account pinned by user ID".into()),
    }
}

fn check_port(leaf: Option<&Operator>, any_address: bool) -> Result<(), String> {
    match leaf {
        Some(op)
            if op.r#type == "simple"
                && op.operand == "dest.port"
                && op.data.bytes().all(|b| b.is_ascii_digit())
                && op
                    .data
                    .parse::<u16>()
                    .is_ok_and(|port| port != 0 && (!any_address || port == DNS_PORT))
                && !op.data.starts_with('0') =>
        {
            Ok(())
        }
        _ => Err("the port condition isn't one port".into()),
    }
}

/// TCP or UDP for a named place; both, for the resolver's any-address rule.
fn check_protocol(leaf: Option<&Operator>, any_address: bool) -> Result<(), String> {
    let allowed: &[Protocol] = if any_address {
        &[Protocol::TcpAndUdp]
    } else {
        &[Protocol::Tcp, Protocol::Udp]
    };
    match leaf {
        Some(op)
            if op.r#type == "regexp"
                && op.operand == "protocol"
                && allowed.iter().any(|protocol| op.data == protocol.pattern()) =>
        {
            Ok(())
        }
        _ => Err("the protocol condition isn't the one this entry may have".into()),
    }
}

#[cfg(test)]
mod dns_policy_tests;
#[cfg(test)]
mod dns_tests;
#[cfg(test)]
mod tests;
