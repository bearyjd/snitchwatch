//! Translate a user-supplied `Verdict` (allow / deny) plus its requested
//! [`VerdictDuration`] and [`VerdictScope`] into the `Rule` proto shape
//! opensnitchd expects as the `AskRule` reply.
//!
//! **Threat model:** `conn.dst_host` is attacker-controlled, arbitrary bytes.
//! Any unprivileged local process can `getaddrinfo("any string")` and that
//! literal string becomes `dst_host`
//! (`vendor/opensnitch/daemon/dns/ebpfhook.go:273` captures the raw eBPF
//! argument with no validation; `vendor/opensnitch/daemon/dns/track.go:47`
//! takes DNS answer names straight from a hostile server). Every function in
//! this module that touches `dst_host` must treat it as hostile input.
//! `conn.process_path` is NOT in this class — the daemon reports it, normally
//! from `/proc/<pid>/exe`. It is not a verified program identity, though:
//! it's the path as seen in the process's own mount namespace, and when
//! `exe` can't be read the daemon falls back to the process-settable `comm`,
//! argv\[0\], or a placeholder such as `Kernel connection`
//! (`daemon/procmon/details.go`). Rules bound to it are as strong as that.
//!
//! The M0 spike taught us things about `Rule`:
//!   - `created` is a unix-seconds int64.
//!   - `duration` is a plain string (`Rule.duration`, proto field 8) — see
//!     [`VerdictDuration::daemon_duration_str`] for the exact mapping from the
//!     pending-decision dialog's four duration options to opensnitchd's
//!     `once` / `<Go duration>` / `until restart` / `always` vocabulary.
//!   - `name` does NOT need to be non-empty — there is no such check
//!     anywhere in `vendor/opensnitch/daemon/rule/*.go` (an empty name would
//!     just yield a rule file literally named `.json`). What *does* matter:
//!     the daemon uses `Rule.name` verbatim as a filename
//!     (`vendor/opensnitch/daemon/rule/loader.go:162`,
//!     `filepath.Join(l.Path, fmt.Sprintf("%s.json", rule.Name))`, written
//!     root-owned 0600; `deleteRuleFromDisk` at loader.go:294 is worse — bare
//!     string concatenation, no `filepath.Join` at all). Since the name is
//!     built from `dst_host` (attacker-controlled — see module doc above),
//!     [`rule_name_for`] sanitizes it before it ever reaches a filename; see
//!     its doc comment (issue #14 security review, FIX 1).
//!
//! A fourth thing, discovered by issue #14: `operator` is **not** optional in
//! practice. `vendor/opensnitch/daemon/rule/rule.go`'s `Deserialize` hard-
//! rejects any `Rule` with `Operator == nil` ("invalid operator"), which
//! makes `ui/client.go`'s `Ask()` return `nil` and the daemon fall through to
//! `DefaultAction` — every verdict this bridge ever sent was silently
//! discarded before this fix. [`build_operator_checked`] is the single place
//! that turns a [`VerdictScope`] into a real `Operator`, using the daemon's
//! own type/operand vocabulary (`vendor/opensnitch/daemon/rule/operator.go`):
//! `Type` is `"simple"`/`"regexp"`, `Operand` is `"dest.host"`/`"dest.ip"`/
//! `"process.path"`.

use super::process_binding::{bindable_process_path, is_bindable_process_path, RuleRefusal};
use crate::cache::connections::Verdict;
use crate::ws_messages::{VerdictDuration, VerdictScope};
use snitchwatch_proto::protocol::{Connection, Operator, Rule};
use std::net::IpAddr;

/// The action token used both in the synthetic rule name and as the `Rule`
/// proto's `action` field.
pub fn verdict_action_str(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Allow => "allow",
        Verdict::Deny => "deny",
    }
}

/// Sanitize an attacker-controlled hostname component before it becomes part
/// of a `Rule.name` — see the module doc and issue #14 security review FIX 1.
///
/// `host` here is `conn.dst_host`/`conn.dst_ip` (see module doc: hostile
/// input). The daemon writes `Rule.name` verbatim to
/// `<rules-dir>/<name>.json` with no validation of its own
/// (`vendor/opensnitch/daemon/rule/loader.go:162`, `deleteRuleFromDisk` at
/// loader.go:294 is worse — no `filepath.Join` at all, pure string
/// concatenation), running as root. An attacker who controls what a local
/// process resolves (`getaddrinfo`) can make `dst_host` literally
/// `"../../../../etc/cron.d/x"`; if the user clicks Allow/Forever on that
/// row, root would write into `/etc/cron.d/`.
///
/// Maps any byte outside `[A-Za-z0-9.-]` to `_` (in particular, `/` — the
/// only thing a single path component actually needs neutralized to block
/// traversal) and trims leading/trailing `.`/`-` for readability, but that
/// alone is many-to-one (`_dmarc.example.com`, `/dmarc.example.com`, and
/// `%dmarc.example.com` would all sanitize to the same string; so would
/// `evil.com`, `evil.com.`, and `evil.com--`). Since the daemon's
/// `addUserRule`/`Add` key persisted rules by this exact name
/// (`vendor/opensnitch/daemon/rule/loader.go`) and always-on-disk rules
/// persist across restarts, a collision here would let one host's saved
/// `Deny`/`Always` rule silently overwrite another's on a later verdict —
/// round 2 of the issue #14 security review flagged this as MEDIUM-2.
/// So injectivity can't rely on the cleaned text alone: a SHA-256 digest
/// of the **raw, unsanitized** bytes is always appended (SHA-256, not
/// `std::collections::hash_map::DefaultHasher` — the old fallback-only
/// version of this function used `DefaultHasher`, which is both
/// non-cryptographic, SipHash-1-3 with an all-zero key, not collision-
/// resistant, and explicitly documented as unstable across Rust releases,
/// so a saved rule's name wasn't even reproducible after a toolchain
/// bump).
fn sanitize_host_for_rule_name(raw: &str) -> String {
    // Leaves room for the always-appended `-<16 hex chars>` digest suffix;
    // purely cosmetic (the daemon has no documented rule-name length
    // limit), not a security boundary.
    const MAX_BASE_LEN: usize = 48;
    rule_name_component(raw, raw, "host", MAX_BASE_LEN)
}

/// Issue #44: the program part of an app-bound rule's name. The readable
/// part is the executable's basename; the digest covers the **full raw
/// path**, so `/usr/bin/curl` and `/tmp/curl` never share a name.
/// `process_path` comes from the daemon (see module doc), but any local
/// user picks the names of their own binaries — and the daemon can fall back
/// to a process-chosen name — so it gets the same `[A-Za-z0-9.-]`
/// neutralization as a hostile host.
fn sanitize_process_for_rule_name(process_path: &str) -> String {
    const MAX_BASE_LEN: usize = 32;
    let basename = match process_path.rfind('/') {
        Some(slash) => &process_path[slash + 1..],
        None => process_path,
    };
    rule_name_component(basename, process_path, "process", MAX_BASE_LEN)
}

/// `<display, neutralized and truncated>-<first 16 hex of SHA-256(identity)>`
/// — the shared shape of every rule-name component. See
/// [`sanitize_host_for_rule_name`] for why the digest of the raw identity
/// is always appended (cleaning alone is many-to-one).
fn rule_name_component(
    display: &str,
    identity: &str,
    fallback: &str,
    max_base_len: usize,
) -> String {
    let cleaned: String = display
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == '-');
    // `cleaned` is guaranteed all single-byte ASCII (every char is either
    // passed through only when `is_ascii_alphanumeric()`/`.`/`-`, or
    // replaced with `_`), so byte-slicing at any length is safe — no
    // UTF-8 char-boundary panic risk.
    let base = if trimmed.is_empty() {
        fallback
    } else {
        &trimmed[..trimmed.len().min(max_base_len)]
    };

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(identity.as_bytes());
    let digest = hasher.finalize();
    let mut digest_hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        digest_hex.push_str(&format!("{byte:02x}"));
    }

    format!("{base}-{digest_hex}")
}

/// Build the synthetic once-off rule name the bridge hands back to
/// opensnitchd as the `AskRule` reply for an interactive verdict.
///
/// This is the single source of truth for that name: [`verdict_to_rule`]
/// uses it to build the `Rule` proto, and
/// [`crate::cache::connections::ConnectionCache::resolve`] uses it to
/// populate the resolved row's `matched_rule` for the UI, so the two never
/// drift apart. `host` should already have the empty-dst-host-falls-back-to-
/// dst-ip substitution applied by the caller (both call sites do) — it is
/// still hostile input (see module doc) and is sanitized via
/// [`sanitize_host_for_rule_name`] before it becomes part of a filename.
///
/// `process_path` is the connection's raw `process_path`. Issue #44 binds
/// rules to their program, so a path [`is_bindable_process_path`] accepts
/// appends `-p<program component>`; any other path (empty, a daemon
/// placeholder, a bare comm name) yields exactly the host-only name, which is
/// also the only operator such a connection can get.
pub fn rule_name_for(verdict: Verdict, host: &str, port: u16, process_path: &str) -> String {
    let host_name = format!(
        "{}{}-{port}",
        answer_name_prefix(verdict),
        sanitize_host_for_rule_name(host)
    );
    if is_bindable_process_path(process_path) {
        format!(
            "{host_name}-p{}",
            sanitize_process_for_rule_name(process_path)
        )
    } else {
        host_name
    }
}

/// The start of every rule name [`rule_name_for`] makes for `verdict`.
pub fn answer_name_prefix(verdict: Verdict) -> String {
    format!("snitchwatch-{}-", verdict_action_str(verdict))
}

/// Whether `name` starts like a name [`rule_name_for`] makes.
pub fn is_answer_name(name: &str) -> bool {
    [Verdict::Allow, Verdict::Deny]
        .into_iter()
        .any(|verdict| name.starts_with(&answer_name_prefix(verdict)))
}

/// The `AskRule` reply for an interactive verdict.
///
/// Issue #44: a remembered rule (any duration but `Once`) must be bound to an
/// absolute executable path, so for a connection without one this returns
/// [`RuleRefusal::ProcessFileUnknown`] instead of degrading to a rule that
/// matches every program. The caller then answers with [`once_rule`].
pub fn verdict_to_rule(
    verdict: Verdict,
    duration: VerdictDuration,
    scope: VerdictScope,
    conn: &Connection,
    now_secs: i64,
) -> Result<Rule, RuleRefusal> {
    if duration.remembers() && bindable_process_path(conn).is_none() {
        return Err(RuleRefusal::ProcessFileUnknown);
    }
    Ok(build_rule(verdict, duration, scope, conn, now_secs))
}

/// A one-shot reply: the daemon applies it to this connection and never
/// stores it (`vendor:daemon/rule/loader.go` `addUserRule`). Infallible, for
/// the paused auto-allow and for answering a [`RuleRefusal`].
pub fn once_rule(verdict: Verdict, scope: VerdictScope, conn: &Connection, now_secs: i64) -> Rule {
    build_rule(verdict, VerdictDuration::Once, scope, conn, now_secs)
}

fn build_rule(
    verdict: Verdict,
    duration: VerdictDuration,
    scope: VerdictScope,
    conn: &Connection,
    now_secs: i64,
) -> Rule {
    let action = verdict_action_str(verdict);

    let host = if conn.dst_host.is_empty() {
        conn.dst_ip.as_str()
    } else {
        conn.dst_host.as_str()
    };

    let (operator, _degradation) = build_operator_checked(scope, conn);

    Rule {
        created: now_secs,
        name: rule_name_for(verdict, host, conn.dst_port as u16, &conn.process_path),
        description: "snitchwatch interactive verdict".to_string(),
        enabled: true,
        precedence: false,
        nolog: false,
        action: action.to_string(),
        duration: duration.daemon_duration_str().to_string(),
        operator: Some(operator),
    }
}

/// Why a `Deny` verdict's requested scope couldn't be honored and
/// [`build_operator_checked`] silently degraded to an exact-host/process-
/// path fallback — see [`scope_degradation`].
///
/// Deliberately carries **no** attacker-controlled data. An earlier version
/// of this type was a raw `String` built with `format!("host \`{host}\`
/// ...")`, interpolating `conn.dst_host` directly — round 2 of the issue
/// #14 security review (MEDIUM-1) flagged that this string then flowed,
/// unsanitized, into a desktop-notification body (freedesktop notification
/// bodies render a markup subset: `<b>`, `<i>`, `<a href>`, `<img src>`,
/// on GNOME/KDE) and into `bridge-cli`'s terminal logs (ANSI/terminal
/// escape sequences), with no length cap. Before this branch, no
/// attacker-controlled string reached a notification body at all.
/// [`Self::describe`] returns a fixed `&'static str` per variant — nothing
/// here is ever built with `format!` from connection data. Where a UI
/// genuinely needs to show the offending host (not required by any current
/// consumer), it must go through [`sanitize_for_display`] at that specific
/// display boundary, not be embedded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeDegradation {
    /// The connection carries no resolved hostname (`dst_host` is empty).
    NoHostRecorded,
    /// `dst_host` is a bare IP address, not a hostname.
    BareIpAddress,
    /// `dst_host` failed [`is_valid_hostname_shape`] or DNS-name parsing.
    InvalidHostnameShape,
    /// `dst_host` has no label beyond its own registrable domain (eTLD+1) —
    /// see [`any_host_on_domain_operator_checked`]'s doc comment.
    AtOrAboveRegistrableDomain,
    /// `conn.process_path` is not an absolute path (see
    /// `process_binding::is_bindable_process_path`).
    ProcessPathUnavailable,
}

impl ScopeDegradation {
    /// A fixed, safe-to-display description — never built from connection
    /// data. See this enum's doc comment.
    pub fn describe(self) -> &'static str {
        match self {
            Self::NoHostRecorded => "no destination host was recorded for this connection",
            Self::BareIpAddress => "the destination is a bare IP address, not a hostname",
            Self::InvalidHostnameShape => "the destination hostname failed validation",
            Self::AtOrAboveRegistrableDomain => {
                "the destination host has no subdomain that can be safely wildcarded below its \
                 public suffix"
            }
            Self::ProcessPathUnavailable => {
                "the process path was not available for this connection"
            }
        }
    }
}

/// Non-`None` exactly when `scope` couldn't be honored as requested and
/// [`build_operator_checked`] silently degraded to an exact-host/process-path
/// fallback. Only meaningful for `Deny` — see issue #14 security review
/// FIX 2: a narrower `Allow` is fail-safe (the user still only gets what they
/// explicitly asked for, just less of it, so [`verdict_to_rule`] applies it
/// silently), but a narrower `Deny` silently *under-blocks* relative to what
/// the pending-decision dialog offered. The caller (the gRPC `ask_rule`
/// handler) uses this to tell the client the block was scoped down, rather
/// than let the UI imply the wider block succeeded.
pub fn scope_degradation(
    scope: VerdictScope,
    verdict: Verdict,
    conn: &Connection,
) -> Option<ScopeDegradation> {
    if verdict != Verdict::Deny {
        return None;
    }
    build_operator_checked(scope, conn).1
}

pub use super::display::{sanitize_for_display, strip_display_hazards};

fn simple_operator(operand: &str, data: &str) -> Operator {
    Operator {
        r#type: "simple".to_string(),
        operand: operand.to_string(),
        data: data.to_string(),
        sensitive: false,
        list: Vec::new(),
    }
}

/// `ThisHost` scope: match the exact destination host, falling back to the
/// destination IP when the connection carries no resolved hostname (a bare-
/// IP connection has an empty `dst_host`). This never "degrades" — it's the
/// fallback everything else degrades to.
fn this_host_operator(conn: &Connection) -> Operator {
    if conn.dst_host.is_empty() {
        simple_operator("dest.ip", conn.dst_ip.as_str())
    } else {
        simple_operator("dest.host", conn.dst_host.as_str())
    }
}

/// Hostname shape validation before it's trusted to build a wildcard
/// pattern — issue #14 security review FIX 3(a). `host` is hostile input
/// (see module doc). Deliberately stricter than DNS actually requires
/// (rejects a leading/trailing dot outright, rather than the "normalize and
/// hope" a permissive DNS-name parser would do) because the goal here isn't
/// "is this syntactically a legal DNS name" but "is this simple and
/// unambiguous enough that dropping its leftmost label is safe to reason
/// about" — a leading dot (`.example.com`, 3 `split('.')` parts, one of them
/// empty) or a trailing dot (`example.com.`, which the daemon would end up
/// matching against `dest.host` values that never carry a trailing dot,
/// silently matching nothing) are exactly the malformed shapes that let a
/// naive label-count guard be bypassed.
fn is_valid_hostname_shape(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// `AnyHostOnDomain` scope: wildcard the leftmost label of the destination
/// host, matching any subdomain of the parent domain (but not the bare
/// parent domain itself, nor the exact original host again) — and, per FIX 3
/// of the issue #14 security review, the apex of the parent domain too (the
/// anchor is `^(?:[^.]+\.)*<parent>$`, not `^.*\.<parent>$`, so a `Deny` at
/// this scope doesn't leave the bare parent domain reachable).
///
/// Returns the reason a degradation happened (if any) alongside the
/// operator — see [`scope_degradation`].
///
/// Degrades to [`this_host_operator`] whenever the wildcard can't be built
/// safely:
///   - no hostname at all, or a bare IP (nothing to safely drop a label
///     from);
///   - a hostname that fails [`is_valid_hostname_shape`] (closes the
///     `.example.com` / `example.com.` guard-bypasses issue #14 flagged —
///     both used to slip past a naive label-count check);
///   - a hostname with no label beyond its own registrable domain (eTLD+1),
///     checked via the `addr` crate's compile-time-embedded Public Suffix
///     List rather than a hand-rolled label-count heuristic. A label-count
///     heuristic alone can't tell `www.example.com` (safe: `com` is a
///     1-label eTLD, `example.com` is the eTLD+1) apart from
///     `user.github.io` (unsafe: `github.io` is ITSELF a 2-label eTLD in the
///     real PSL — private-domain entry for GitHub Pages — so wildcarding
///     `user`'s leftmost label would match every GitHub Pages site) or
///     `shop.co.uk` (unsafe: `co.uk` is a 2-label eTLD, so `shop.co.uk` IS
///     the eTLD+1, nothing safe to wildcard). A too-broad allow/deny rule is
///     a security defect either way (over-allow or a deny that silently
///     covers far more than the user saw), so this function never emits a
///     pattern that isn't scoped to a real, registrable parent domain.
fn any_host_on_domain_operator_checked(conn: &Connection) -> (Operator, Option<ScopeDegradation>) {
    let raw_host = conn.dst_host.as_str();

    if raw_host.is_empty() {
        return (
            this_host_operator(conn),
            Some(ScopeDegradation::NoHostRecorded),
        );
    }
    if raw_host.parse::<IpAddr>().is_ok() {
        return (
            this_host_operator(conn),
            Some(ScopeDegradation::BareIpAddress),
        );
    }

    // CRITICAL (issue #14 security review round 2): lowercase ONCE, here,
    // and use this value for every subsequent step — shape validation, the
    // PSL lookup, AND the emitted pattern's parent-domain split. `psl`
    // matches suffix labels as raw lowercase bytes
    // (`psl-2.1.223/src/list.rs`), and neither `addr::parse_dns_name` nor
    // `psl` lowercases its input. `is_valid_hostname_shape`'s charset
    // permits `A-Z`, so an uppercase host (`USER.GITHUB.IO`) used to reach
    // the PSL lookup unchanged, miss every real suffix entry (which are all
    // stored lowercase), fall through to the PSL's default rule (suffix =
    // last label only), and `.prefix()` would incorrectly return `Some` —
    // passing the gate this function exists to enforce and producing
    // `^(?:[^.]+\.)*github\.io$` for a Deny that was supposed to stay
    // scoped to `user.github.io`. The daemon then lowercases both the
    // pattern (operator.go:146-148) and the value it matches against (same
    // lines) at rule-compile/match time — so in production this pattern
    // would have matched every GitHub Pages site. Any local process can
    // trigger this via `getaddrinfo("USER.GITHUB.IO")` (DNS's 0x20
    // encoding preserves case). Precedent for lowercasing hostile hostname
    // input once at the boundary: `blocklists/materializer.rs:94`.
    //
    // Simple `dest.host`/`dest.ip` operators ([`this_host_operator`],
    // used both as this function's own fallback and directly for
    // `ThisHost` scope) are NOT affected by this class of bug: the daemon
    // compares them case-insensitively (`operator.go:225-226`,
    // `strings.EqualFold`) regardless of what case we send, so they're
    // left using `conn.dst_host`'s original case rather than this
    // lowercased copy.
    let host = raw_host.to_ascii_lowercase();
    let host = host.as_str();

    if !is_valid_hostname_shape(host) {
        return (
            this_host_operator(conn),
            Some(ScopeDegradation::InvalidHostnameShape),
        );
    }

    // `addr` wraps the `psl` crate's compile-time-embedded Mozilla Public
    // Suffix List — no network access, no build.rs (verified: `psl`'s
    // Cargo.toml has `build = false`, `#![no_std]`, and the list is
    // pre-generated Rust source, not fetched at build or run time). `addr`
    // was already a workspace dependency before this fix (declared, unused);
    // reused here instead of adding `psl` directly.
    let parsed = match addr::parse_dns_name(host) {
        Ok(name) => name,
        Err(_) => {
            return (
                this_host_operator(conn),
                Some(ScopeDegradation::InvalidHostnameShape),
            )
        }
    };
    // `.prefix()` is `None` exactly when `host` IS its own registrable
    // domain (eTLD+1) — nothing beyond the registrable owner's label to
    // safely wildcard away. This is what correctly rejects `example.com`
    // (registrable domain = itself), `shop.co.uk` (`co.uk` is the eTLD,
    // `shop.co.uk` is the eTLD+1 = itself), and `user.github.io`
    // (`github.io` is the eTLD, `user.github.io` is the eTLD+1 = itself) —
    // now correctly even when the daemon-observed host arrives uppercase.
    if parsed.prefix().is_none() {
        return (
            this_host_operator(conn),
            Some(ScopeDegradation::AtOrAboveRegistrableDomain),
        );
    }

    // Safe to wildcard: host has at least one label beyond its registrable
    // domain, so dropping only the leftmost label never reaches (or goes
    // above) the registrable owner's boundary. `host` (the lowercased
    // copy) is what gets split here, not `raw_host` — the whole point of
    // lowercasing once at the top is that every downstream step, including
    // this one, sees a consistent value.
    let labels: Vec<&str> = host.split('.').collect();
    let parent_domain = labels[1..].join(".");
    let pattern = format!(r"^(?:[^.]+\.)*{}$", regex::escape(&parent_domain));
    (
        Operator {
            r#type: "regexp".to_string(),
            operand: "dest.host".to_string(),
            data: pattern,
            sensitive: false,
            list: Vec::new(),
        },
        None,
    )
}

/// `AnyHost` scope: the daemon has no "match everything" operator (`operator`
/// is mandatory), so this scopes the rule to the process instead — Little
/// Snitch's "allow this app to connect anywhere" semantics. Degrades to
/// [`this_host_operator`] when the process path isn't bindable, rather than
/// risk an operator that matches every connection from every process. Since
/// issue #44's second half that fallback only ever builds a once-only reply:
/// [`verdict_to_rule`] refuses to remember it.
///
/// `conn.process_path` is reported by the daemon, NOT hostile input like
/// `dst_host` — see module doc for what it is and isn't.
fn any_host_operator_checked(conn: &Connection) -> (Operator, Option<ScopeDegradation>) {
    if bindable_process_path(conn).is_none() {
        tracing::warn!(
            dst_host = %conn.dst_host,
            dst_ip = %conn.dst_ip,
            "AnyHost verdict scope requested without an absolute process_path; \
             degrading to ThisHost rather than emit an unscoped rule"
        );
        return (
            this_host_operator(conn),
            Some(ScopeDegradation::ProcessPathUnavailable),
        );
    }
    (process_path_operator(conn), None)
}

/// Exact, **case-sensitive** match on the executable path. A non-sensitive
/// simple operator compares with Unicode case folding (`strings.EqualFold`,
/// `operator.go` `simpleCmp`), which Linux paths don't have — `/tmp/ſcript`
/// would match a rule for `/tmp/script`.
fn process_path_operator(conn: &Connection) -> Operator {
    Operator {
        sensitive: true,
        ..simple_operator("process.path", conn.process_path.as_str())
    }
}

fn build_operator_checked(
    scope: VerdictScope,
    conn: &Connection,
) -> (Operator, Option<ScopeDegradation>) {
    match scope {
        VerdictScope::ThisHost => (bind_to_process(this_host_operator(conn), conn), None),
        VerdictScope::AnyHostOnDomain => {
            let (host, degradation) = any_host_on_domain_operator_checked(conn);
            (bind_to_process(host, conn), degradation)
        }
        VerdictScope::AnyHost => any_host_operator_checked(conn),
    }
}

/// Issue #44: a `ThisHost` / `AnyHostOnDomain` answer is about *this*
/// program reaching that host, so the rule must match the program AND the
/// host. A bare host operator matches every process, which turned one app's
/// remembered allow into an allow for all of them.
///
/// opensnitchd ANDs `list` members (`daemon/rule/operator.go` `listMatch`,
/// short-circuiting, so the exact `process.path` compare goes first),
/// compiles each member when the rule is loaded, and ignores `data` for a
/// `list`. This binds to the executable *path* the daemon reports, which is
/// as strong as the daemon's process identity and no stronger — see module
/// doc.
///
/// A path that isn't bindable keeps the bare host operator. That is only
/// reachable for a once-only reply: [`verdict_to_rule`] refuses to remember
/// such a rule (issue #44, second half).
fn bind_to_process(host: Operator, conn: &Connection) -> Operator {
    if bindable_process_path(conn).is_none() {
        tracing::debug!("host-scoped once-only reply without an absolute process_path");
        return host;
    }
    Operator {
        r#type: "list".to_string(),
        operand: "list".to_string(),
        data: String::new(),
        sensitive: false,
        list: vec![process_path_operator(conn), host],
    }
}

/// Issue #44, second half: the absolute-path gate (kept out of `tests` below,
/// which already holds the rest of this module's cases).
#[cfg(test)]
mod refusal_tests;

#[cfg(test)]
mod tests;
