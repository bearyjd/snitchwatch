use crate::MockError;
use snitchwatch_proto::protocol::Rule;

// Line cites into `vendor/opensnitch` here and in `tests/rule_shape.rs` are
// v1.8.0's numbering (the daemon this mock models) unless marked master; the
// submodule is at master a1353848, where `operator.go`/`loader.go` moved.

/// Reject the class of malformed `Rule` a real daemon rejects — mirroring
/// its ACTUAL acceptance path, not just `rule.Deserialize`. Two prior
/// versions of this function only checked `Deserialize`
/// (`vendor/opensnitch/daemon/rule/rule.go:85-89`, `operator: None` only),
/// but real rejection mostly happens later, in `Operator.Compile()`
/// (`vendor/opensnitch/daemon/rule/operator.go:109-214`, called from
/// `loader.go:408` when the daemon loads/applies the rule) — `Deserialize`'s
/// own `NewOperator` call never errors. See issue #14 security review FIX 4.
///
/// Checks, in the order the daemon would effectively hit them:
///   1. `operator: None` (`Deserialize`, rule.go:85-89).
///   2. Unknown `operator.type` (`Compile`, operator.go:207 — the final
///      `else` branch, "Unknown Operator type").
///   3. A `regexp`-typed operator whose `data` doesn't compile
///      (`Compile`, operator.go:146-149) — lowercased first when
///      `sensitive == false`, exactly as the daemon does, since case
///      matters for what actually gets handed to `regexp.Compile`.
///   4. `rule.duration` that's neither a named duration (`once`,
///      `until restart`, `always`) nor `time.ParseDuration`-parseable
///      (`loader.go:326` `isTemporary`, `loader.go:441`
///      `scheduleTemporaryRule`'s `time.ParseDuration` call).
///   5. An unsafe/empty rule name (`loader.go:162`) — this is a
///      bridge-side contract stricter than what the daemon itself enforces
///      (it has no such check at all, which is exactly issue #14 security
///      review FIX 1's finding), kept here as a regression canary: this
///      check is what would have caught FIX 1 at test time.
// `MockError::Rpc(tonic::Status)` is already the large variant driving this;
// the crate's public `MockError`-returning methods (`ask_rule`, `ping`, ...)
// are exempted from this lint as public API, so this private helper needs
// the same allowance rather than boxing just for its own sake.
#[allow(clippy::result_large_err)]
pub fn validate_rule_shape(rule: &Rule) -> Result<(), MockError> {
    validate_rule_shape_for(rule, DEFAULT_VERSION)
}

/// [`validate_rule_shape`] for a daemon reporting `version`: from 1.9
/// (upstream master's `Compile`, `operator.go:186-188` there) a `network`
/// condition compiles only with `dest.network`. An unreadable version is
/// v1.8.0's rules, as the bridge reads it.
#[allow(clippy::result_large_err)]
pub fn validate_rule_shape_for(rule: &Rule, version: &str) -> Result<(), MockError> {
    validate_rule_name(&rule.name)?;
    validate_duration(&rule.duration)?;

    let operator = rule
        .operator
        .as_ref()
        .ok_or_else(|| MockError::InvalidRule("operator is None".to_string()))?;
    validate_operator_compiles(operator, dest_network_only(version))?;
    Ok(())
}

/// The version a mock reports unless a test picks another (the shipped
/// daemon's).
pub const DEFAULT_VERSION: &str = "1.8.0";

/// Whether a daemon reporting `version` (its first `major.minor`) is 1.9 or
/// newer.
pub fn dest_network_only(version: &str) -> bool {
    static MAJOR_MINOR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re =
        MAJOR_MINOR.get_or_init(|| regex::Regex::new(r"([0-9]+)\.([0-9]+)").expect("valid regex"));
    re.captures(version)
        .and_then(|c| Some((c[1].parse::<u32>().ok()?, c[2].parse::<u32>().ok()?)))
        .is_some_and(|major_minor| major_minor >= (1, 9))
}

/// The `operator.type` vocabulary the daemon actually recognizes
/// (`vendor/opensnitch/daemon/rule/operator.go`'s `Type` consts: `Simple`,
/// `Regexp`, `Complex`, `List`, `Network`, `Lists`).
const KNOWN_OPERATOR_TYPES: &[&str] = &["simple", "regexp", "complex", "list", "network", "lists"];

/// The `operator.operand` vocabulary the daemon recognizes
/// (`vendor/opensnitch/daemon/rule/operator.go`'s `Operand` consts,
/// lines 31-56). `process.env.` is a *prefix* (`OpProcessEnvPrefix`), not an
/// exact value — `process.env.PATH` is a valid operand — so it's checked
/// separately in [`is_known_operand`], not listed here verbatim.
const KNOWN_OPERANDS: &[&str] = &[
    "true",
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
    "dest.network",
    "source.network",
    "protocol",
    "iface.in",
    "iface.out",
    "list",
    "lists.domains",
    "lists.domains_regexp",
    "lists.ips",
    "lists.nets",
    "lists.hash.md5",
];

/// Operands `Match` evaluates against a `net.IP` rather than a string
/// (operator.go `Match`: `OpDstNetwork`, `OpSrcNetwork`, `OpNetLists`).
const IP_OPERANDS: &[&str] = &["dest.network", "source.network", "lists.nets"];

/// `lists.ips` outside the `lists` type: v1.8.0 compiles it (its `Match`
/// compares `con.DstIP.String()`, a string), and only upstream master's
/// `cbGeneric` path panics on it. The mock refuses it at every version on
/// purpose, so the bridge never relies on either. Not an [`IP_OPERANDS`]
/// member, which would let the `network` type (`cmpNetwork`'s
/// `value.(net.IP)`) take it.
const LISTS_ONLY_OPERANDS: &[&str] = &["lists.ips"];

fn is_known_operand(operand: &str) -> bool {
    KNOWN_OPERANDS.contains(&operand) || operand.starts_with("process.env.")
}

/// Mirrors `Operator.Compile()` (operator.go:109-214) for the subset this
/// bridge actually emits (`simple`/`regexp`, and since issue #44 a `list`
/// of those — see `translator::verdict`).
/// Not a full port: `network`/`lists` compilation depends on daemon-local
/// state (alias cache, loaded blocklists) this mock has no access to, and
/// this bridge never emits those types for an `AskRule` reply, so they're
/// only checked for a known type, not fully compiled.
///
/// Issue #14 security review round 2, LOW: `Operator.Compile()`
/// (`operator.go:109-214`) doesn't actually validate `Operand` against this
/// vocabulary for every `Type` — for `Simple`, `Compile` sets its callback
/// unconditionally regardless of what `Operand` says. An unknown/typo'd
/// operand still "compiles" successfully; the daemon only discovers the
/// problem when a *later*, separate operand-to-connection-field dispatch
/// (outside `operator.go`) falls through to nothing for the value, so the
/// rule silently never matches and every connection through it falls
/// through to `DefaultAction` — structurally the same false-pass class as
/// issue #14 itself (a `Rule` that "looks" accepted but never actually
/// applies). This check is stricter than `Compile()` on purpose, as a test-
/// time canary for that failure mode, mirroring [`validate_rule_name`]'s
/// FIX-1-canary role.
#[allow(clippy::result_large_err)]
fn validate_operator_compiles(
    op: &snitchwatch_proto::protocol::Operator,
    dest_network_only: bool,
) -> Result<(), MockError> {
    if !KNOWN_OPERATOR_TYPES.contains(&op.r#type.as_str()) {
        return Err(MockError::InvalidRule(format!(
            "unknown operator type: `{}`",
            op.r#type
        )));
    }
    // `Compile` overwrites a list's operand with `OpList` (operator.go:154-155),
    // so the daemon accepts any operand there — including the empty one the
    // bridge's `operator_from_wire` produces for a GUI-toggled list rule.
    if op.r#type != "list" && !is_known_operand(&op.operand) {
        return Err(MockError::InvalidRule(format!(
            "unknown operator operand: `{}`",
            op.operand
        )));
    }

    if op.r#type == "regexp" {
        // operator.go:146-148: lowercased before compiling when
        // Sensitive == false — case affects what actually gets compiled.
        let data = if op.sensitive {
            op.data.clone()
        } else {
            op.data.to_lowercase()
        };
        if let Err(e) = regex::Regex::new(&data) {
            return Err(MockError::InvalidRule(format!(
                "operator.data does not compile as a regexp: {e}"
            )));
        }
    }

    // `Match` hands these operands' callbacks a `net.IP` and every other
    // operand a string, while the callback (chosen by the type) type-asserts
    // with no recover: `simpleCmp` does `v.(string)`, `cmpNetwork` and the
    // alias callback `value.(net.IP)`. A mismatch panics the real daemon on
    // the first matching connection, so it must never pass here.
    let ip_operand = IP_OPERANDS.contains(&op.operand.as_str());
    if op.r#type == "network" && !ip_operand {
        return Err(MockError::InvalidRule(
            "network type with a string operand would panic the daemon".to_string(),
        ));
    }
    if ip_operand && op.r#type != "network" && op.r#type != "lists" {
        return Err(MockError::InvalidRule(
            "an IP operand without the network type would panic the daemon or never match"
                .to_string(),
        ));
    }
    if LISTS_ONLY_OPERANDS.contains(&op.operand.as_str()) && op.r#type != "lists" {
        return Err(MockError::InvalidRule(
            "a lists.ips operand without the lists type panics upstream master (refused at every version)"
                .to_string(),
        ));
    }
    if dest_network_only && op.r#type == "network" && op.operand == "source.network" {
        return Err(MockError::InvalidRule(
            "opensnitchd 1.9 and newer only accepts dest.network for a network condition"
                .to_string(),
        ));
    }

    // `Deserialize` (rule.go:113-127) keeps members only for the `list`
    // type, and only one level deep.
    if op.r#type != "list" && !op.list.is_empty() {
        return Err(MockError::InvalidRule(
            "members of a non-list operator are dropped by the daemon".to_string(),
        ));
    }

    // `Match` dispatches on the operand (operator.go:340), not the type, so
    // guard either spelling of a list.
    if op.r#type == "list" || op.operand == "list" {
        // loader.go:413-420 compiles every member of a `list`, and
        // listMatch (operator.go:327-333) starts from `true`, so a list with
        // no members would match every connection.
        if op.list.is_empty() {
            return Err(MockError::InvalidRule(
                "list operator has no members".to_string(),
            ));
        }
        for member in &op.list {
            if member.r#type == "list" || member.operand == "list" {
                return Err(MockError::InvalidRule(
                    "nested list: the daemon copies members one level deep, so the inner list \
                     is empty and matches every connection"
                        .to_string(),
                ));
            }
            validate_operator_compiles(member, dest_network_only)?;
        }
    }

    Ok(())
}

/// The daemon's three named `Rule.duration` values
/// (`vendor/opensnitch/daemon/rule/rule.go:31-34`).
const NAMED_DURATIONS: &[&str] = &["once", "until restart", "always"];

#[allow(clippy::result_large_err)]
pub(crate) fn validate_duration(duration: &str) -> Result<(), MockError> {
    if NAMED_DURATIONS.contains(&duration) {
        return Ok(());
    }
    if looks_like_go_duration(duration) {
        return Ok(());
    }
    Err(MockError::InvalidRule(format!(
        "duration `{duration}` is neither a named duration ({NAMED_DURATIONS:?}) nor \
         Go-duration-parseable"
    )))
}

/// Approximates Go's `time.ParseDuration` grammar closely enough to catch
/// the malformed-duration class of bug: optional sign, then one or more
/// `<number><unit>` segments (`ns`/`us`/`µs`/`ms`/`s`/`m`/`h`), or the bare
/// literal `0` (which Go's parser special-cases as valid with no unit). This
/// is NOT a byte-for-byte port of Go's parser — it doesn't need to be, only
/// to reject/accept the same shapes this bridge could plausibly produce or
/// regress into.
fn looks_like_go_duration(s: &str) -> bool {
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    if unsigned.is_empty() {
        return false;
    }
    if unsigned == "0" {
        return true;
    }
    static GO_DURATION_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = GO_DURATION_RE.get_or_init(|| {
        regex::Regex::new(r"^([0-9]+(\.[0-9]*)?(ns|us|µs|ms|s|m|h))+$").expect("valid regex")
    });
    re.is_match(unsigned)
}

/// Regression canary for issue #14 security review FIX 1: the daemon has NO
/// rule-name validation of its own (it writes `Rule.name` verbatim to
/// `<rules-dir>/<name>.json`, root-owned — see `validate_rule_shape`'s doc),
/// so this bridge-side check is stricter than the real acceptance path on
/// purpose. It exists to make a regression in
/// `translator::verdict::sanitize_host_for_rule_name` fail a test instead of
/// silently reintroducing a path-traversal-via-rule-name bug.
#[allow(clippy::result_large_err)]
fn validate_rule_name(name: &str) -> Result<(), MockError> {
    if name.is_empty() {
        return Err(MockError::InvalidRule("rule name is empty".to_string()));
    }
    if name.contains('/') {
        return Err(MockError::InvalidRule(format!(
            "rule name contains a path separator: `{name}`"
        )));
    }
    Ok(())
}
