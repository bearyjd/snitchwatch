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
//! `conn.process_path` is NOT in this class — it's daemon-attested from
//! `/proc/<pid>/exe`.
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

    let cleaned: String = raw
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
        "host"
    } else {
        &trimmed[..trimmed.len().min(MAX_BASE_LEN)]
    };

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
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
pub fn rule_name_for(verdict: Verdict, host: &str, port: u16) -> String {
    format!(
        "snitchwatch-{}-{}-{port}",
        verdict_action_str(verdict),
        sanitize_host_for_rule_name(host)
    )
}

pub fn verdict_to_rule(
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
        name: rule_name_for(verdict, host, conn.dst_port as u16),
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
    /// `conn.process_path` is empty.
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

/// Sanitize an attacker-controlled string before it's shown in a desktop
/// notification body or sent to the WS client as protocol text — issue #14
/// security review round 2, MEDIUM-1. See [`ScopeDegradation`]'s doc
/// comment for why this exists. Strips control characters (including the
/// ANSI `ESC` byte, newlines, carriage returns — bridge-cli logs apply
/// terminal escape sequences), HTML-entity-escapes the markup
/// metacharacters `<`/`>`/`&` (so a literal `<b>` in a hostname displays as
/// the text `<b>` rather than being interpreted as bold by a freedesktop
/// notification daemon), and caps the result to `max_len` **characters**
/// (not bytes — truncating mid-codepoint would corrupt multi-byte UTF-8).
pub fn sanitize_for_display(input: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for c in input.chars() {
        if count >= max_len {
            out.push('…');
            break;
        }
        if c.is_control() || is_display_hazard(c) {
            continue;
        }
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            other => out.push(other),
        }
        count += 1;
    }
    out
}

/// Unicode characters `char::is_control()` (category Cc only) doesn't
/// catch, but that still let attacker-controlled text visually lie about
/// itself once rendered in a notification body or log — issue #14 security
/// review round 2, LOW. Bidi format controls (category Cf) can reorder or
/// hide surrounding text: e.g. a hostname containing U+202E
/// RIGHT-TO-LEFT OVERRIDE can make the *displayed* text read as a
/// different, more trustworthy-looking domain than the bytes actually are.
/// The zero-width/invisible-joiner controls (also Cf) can hide characters
/// entirely or defeat naive substring-based review. The line/paragraph
/// separators (category Zl/Zp) can inject a visual line break a
/// control-char-only strip wouldn't catch, splitting a notification body
/// across lines the caller didn't intend.
fn is_display_hazard(c: char) -> bool {
    matches!(c,
        '\u{200B}'..='\u{200F}' // zero-width space/ZWNJ/ZWJ, LRM, RLM
        | '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
        | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
        | '\u{FEFF}' // BOM / zero-width no-break space
        | '\u{2028}' // LINE SEPARATOR
        | '\u{2029}' // PARAGRAPH SEPARATOR
    )
}

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
/// [`this_host_operator`] when the process path is unknown, rather than risk
/// an operator that matches every connection from every process.
///
/// `conn.process_path` is daemon-attested (`/proc/<pid>/exe`), NOT hostile
/// input like `dst_host` — see module doc.
fn any_host_operator_checked(conn: &Connection) -> (Operator, Option<ScopeDegradation>) {
    if conn.process_path.is_empty() {
        tracing::warn!(
            dst_host = %conn.dst_host,
            dst_ip = %conn.dst_ip,
            "AnyHost verdict scope requested with an empty process_path; \
             degrading to ThisHost rather than emit an unscoped rule"
        );
        return (
            this_host_operator(conn),
            Some(ScopeDegradation::ProcessPathUnavailable),
        );
    }
    (
        simple_operator("process.path", conn.process_path.as_str()),
        None,
    )
}

fn build_operator_checked(
    scope: VerdictScope,
    conn: &Connection,
) -> (Operator, Option<ScopeDegradation>) {
    match scope {
        VerdictScope::ThisHost => (this_host_operator(conn), None),
        VerdictScope::AnyHostOnDomain => any_host_on_domain_operator_checked(conn),
        VerdictScope::AnyHost => any_host_operator_checked(conn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_connection() -> Connection {
        Connection {
            protocol: "tcp".to_string(),
            dst_ip: "140.82.121.4".to_string(),
            dst_host: "github.com".to_string(),
            dst_port: 443,
            process_path: "/usr/bin/curl".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn allow_verdict_produces_allow_rule_with_once_duration() {
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &sample_connection(),
            1_700_000_000,
        );
        assert_eq!(rule.action, "allow");
        assert_eq!(rule.duration, "once");
        assert!(rule.enabled);
        assert_eq!(rule.created, 1_700_000_000);
        assert!(rule.name.contains("allow"));
    }

    #[test]
    fn deny_verdict_produces_deny_rule() {
        let rule = verdict_to_rule(
            Verdict::Deny,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &sample_connection(),
            1_700_000_000,
        );
        assert_eq!(rule.action, "deny");
        assert_eq!(rule.duration, "once");
        assert!(rule.name.contains("deny"));
    }

    #[test]
    fn rule_name_includes_remote_host_for_traceability() {
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &sample_connection(),
            0,
        );
        assert!(rule.name.contains("github.com"), "got: {}", rule.name);
    }

    #[test]
    fn rule_name_for_matches_verdict_to_rule_name() {
        // Single source of truth: the name the daemon gets back (via
        // `verdict_to_rule`) and the name the UI shows as `matched_rule` (via
        // `rule_name_for`, called from `ConnectionCache::resolve`) must be
        // identical for the same inputs, or the "Show rule" jump would land
        // on a rule that doesn't exist.
        let conn = sample_connection();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &conn,
            1_700_000_000,
        );
        let name = rule_name_for(Verdict::Allow, &conn.dst_host, conn.dst_port as u16);
        assert_eq!(rule.name, name);
    }

    #[test]
    fn each_ui_duration_maps_to_the_expected_daemon_string() {
        let conn = sample_connection();
        let cases = [
            (VerdictDuration::Once, "once"),
            (VerdictDuration::FiveMinutes, "5m"),
            (VerdictDuration::UntilRestart, "until restart"),
            (VerdictDuration::Always, "always"),
        ];
        for (duration, expected) in cases {
            let rule = verdict_to_rule(Verdict::Allow, duration, VerdictScope::ThisHost, &conn, 0);
            assert_eq!(rule.duration, expected, "duration mapping for {duration:?}");
        }
    }

    // -- operator population (issue #14) --------------------------------

    #[test]
    fn every_rule_carries_a_non_none_operator() {
        // The daemon hard-rejects `operator: None` (rule.go's Deserialize),
        // so this must never regress for any scope.
        for scope in [
            VerdictScope::ThisHost,
            VerdictScope::AnyHostOnDomain,
            VerdictScope::AnyHost,
        ] {
            let rule = verdict_to_rule(
                Verdict::Allow,
                VerdictDuration::Once,
                scope,
                &sample_connection(),
                0,
            );
            assert!(
                rule.operator.is_some(),
                "scope {scope:?} produced a None operator"
            );
        }
    }

    #[test]
    fn this_host_scope_matches_on_dest_host() {
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &sample_connection(),
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, "github.com");
    }

    #[test]
    fn this_host_scope_falls_back_to_dest_ip_when_host_is_empty() {
        let mut conn = sample_connection();
        conn.dst_host = String::new();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::ThisHost,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "dest.ip");
        assert_eq!(op.data, "140.82.121.4");
    }

    #[test]
    fn any_host_on_domain_scope_wildcards_the_leftmost_label() {
        let mut conn = sample_connection();
        conn.dst_host = "www.example.com".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "regexp");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, r"^(?:[^.]+\.)*example\.com$");
    }

    #[test]
    fn any_host_on_domain_scope_wildcard_matches_bare_apex_and_subdomains() {
        // FIX 3(c): the tightened anchor must match real label boundaries
        // AND the bare parent domain itself (closing the deny-leaves-apex-
        // reachable gap from FIX 2).
        let re = regex::Regex::new(r"^(?:[^.]+\.)*example\.com$").unwrap();
        assert!(re.is_match("example.com"), "must match the bare apex");
        assert!(re.is_match("www.example.com"));
        assert!(re.is_match("a.b.example.com"), "nested subdomains");
        assert!(!re.is_match("notexample.com"));
        assert!(!re.is_match("example.com.evil.com"));
    }

    #[test]
    fn any_host_on_domain_scope_degrades_for_hostname_with_invalid_characters() {
        // `+` is not in the allowed hostname charset ([A-Za-z0-9_-]) —
        // `is_valid_hostname_shape` rejects it, so this degrades to an exact
        // match rather than ever reaching regex-pattern construction. This
        // is what actually prevents regex-metacharacter injection: no valid
        // (post-validation) hostname can contain an unescaped-needing
        // character in the first place, since the allowed charset has none.
        let mut conn = sample_connection();
        conn.dst_host = "a.b+c.example.com".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
        assert_eq!(op.operand, "dest.host");
    }

    #[test]
    fn any_host_on_domain_scope_escapes_literal_dots_in_the_parent_domain() {
        // Dots are the one "special" character a valid hostname legitimately
        // contains, and they must be escaped as literal dots, not left as
        // the regex any-character metacharacter.
        let mut conn = sample_connection();
        conn.dst_host = "www.example.co.uk".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        let re = regex::Regex::new(&op.data).unwrap();
        assert!(re.is_match("example.co.uk"));
        assert!(
            !re.is_match("exampleXcoXuk"),
            "dots must be literal, not `.`"
        );
    }

    #[test]
    fn any_host_on_domain_scope_degrades_to_this_host_for_single_label_host() {
        let mut conn = sample_connection();
        conn.dst_host = "localhost".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        // Never a bare wildcard-everything pattern.
        assert_ne!(
            op.r#type, "regexp",
            "must not emit a regexp for a single-label host"
        );
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, "localhost");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_to_this_host_for_two_label_host() {
        // `example.com` (via the `github.com` fixture) IS its own
        // registrable domain (eTLD+1) — nothing to safely wildcard.
        let conn = sample_connection(); // dst_host = "github.com"
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, "github.com");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_to_this_host_for_bare_ip() {
        let mut conn = sample_connection();
        conn.dst_host = "203.0.113.5".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_ne!(
            op.r#type, "regexp",
            "must not emit a regexp for a bare IP host"
        );
    }

    // -- FIX 3(a): guard-bypass hostname shapes --------------------------

    #[test]
    fn any_host_on_domain_scope_degrades_for_leading_dot_host() {
        // ".example.com" splits into 3 parts (["", "example", "com"]),
        // which used to slip past a naive `labels.len() < 3` guard.
        let mut conn = sample_connection();
        conn.dst_host = ".example.com".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_for_trailing_dot_host() {
        // "example.com." would otherwise produce `^.*\.com\.$`-style
        // patterns that match nothing real (a Deny that silently blocks
        // zero traffic).
        let mut conn = sample_connection();
        conn.dst_host = "example.com.".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
    }

    // -- FIX 3(b): real public-suffix boundaries, not a label-count guess -

    #[test]
    fn any_host_on_domain_scope_wildcards_multi_label_etld_correctly() {
        let mut conn = sample_connection();
        conn.dst_host = "www.example.co.uk".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "regexp");
        assert_eq!(op.data, r"^(?:[^.]+\.)*example\.co\.uk$");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_when_host_is_exactly_its_etld_plus_one_multi_label() {
        // `co.uk` is a 2-label eTLD; `shop.co.uk` IS the registrable domain
        // (eTLD+1) — nothing safe to wildcard. A label-count-only guard
        // (labels.len() < 3) would have wrongly allowed this.
        let mut conn = sample_connection();
        conn.dst_host = "shop.co.uk".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
        assert_eq!(op.data, "shop.co.uk");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_for_private_suffix_github_io() {
        // `github.io` is listed in the real PSL as a *private* suffix
        // (GitHub Pages). `user.github.io` is the registrable domain —
        // wildcarding it would match every GitHub Pages site.
        let mut conn = sample_connection();
        conn.dst_host = "user.github.io".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
        assert_eq!(op.data, "user.github.io");
    }

    // -- CRITICAL: uppercase must not bypass the PSL gate (round 2) -------

    #[test]
    fn any_host_on_domain_scope_degrades_for_uppercase_private_suffix_host() {
        // Before the fix, `psl`'s suffix lookup missed every real entry on
        // uppercase input, fell through to the default "last label only"
        // rule, and `.prefix()` incorrectly returned `Some` — this would
        // have wildcarded to `^(?:[^.]+\.)*github\.io$`, matching every
        // GitHub Pages site.
        let mut conn = sample_connection();
        conn.dst_host = "USER.GITHUB.IO".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
    }

    #[test]
    fn any_host_on_domain_scope_degrades_for_uppercase_two_label_etld_host() {
        let mut conn = sample_connection();
        conn.dst_host = "SHOP.CO.UK".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple", "must degrade, not wildcard");
    }

    #[test]
    fn any_host_on_domain_scope_wildcards_mixed_case_host_correctly() {
        // A mixed-case host that legitimately has a subdomain beyond its
        // registrable domain must still wildcard — the fix must not
        // over-correct into degrading every non-lowercase host.
        let mut conn = sample_connection();
        conn.dst_host = "Www.Example.Com".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "regexp");
        assert_eq!(op.data, r"^(?:[^.]+\.)*example\.com$");
    }

    #[test]
    fn any_host_on_domain_scope_wildcards_uppercase_multi_label_etld_correctly() {
        let mut conn = sample_connection();
        conn.dst_host = "WWW.EXAMPLE.CO.UK".to_string();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHostOnDomain,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "regexp");
        assert_eq!(op.data, r"^(?:[^.]+\.)*example\.co\.uk$");
    }

    #[test]
    fn deny_verdict_degradation_is_surfaced_for_uppercase_bypass_host() {
        // Before the fix this returned None (the bypass meant no
        // degradation was ever detected), so the FIX 2 guarantee — a Deny
        // never silently narrows without telling the caller — didn't
        // actually hold for this input. Confirms it does now.
        let mut conn = sample_connection();
        conn.dst_host = "USER.GITHUB.IO".to_string();
        let reason = scope_degradation(VerdictScope::AnyHostOnDomain, Verdict::Deny, &conn);
        assert!(
            reason.is_some(),
            "uppercase bypass must still be caught and surfaced for Deny"
        );
        assert_eq!(reason, Some(ScopeDegradation::AtOrAboveRegistrableDomain));
    }

    // -- FIX 2: Deny must never silently narrow ---------------------------

    #[test]
    fn allow_verdict_degradation_is_silent() {
        let mut conn = sample_connection();
        conn.dst_host = "shop.co.uk".to_string(); // would degrade
        let reason = scope_degradation(VerdictScope::AnyHostOnDomain, Verdict::Allow, &conn);
        assert!(
            reason.is_none(),
            "Allow degradation must not be surfaced (fail-safe narrowing)"
        );
    }

    #[test]
    fn deny_verdict_degradation_is_surfaced() {
        let mut conn = sample_connection();
        conn.dst_host = "shop.co.uk".to_string(); // would degrade
        let reason = scope_degradation(VerdictScope::AnyHostOnDomain, Verdict::Deny, &conn);
        assert!(
            reason.is_some(),
            "Deny degradation must be surfaced — silent under-blocking is a defect"
        );
    }

    #[test]
    fn deny_verdict_with_no_degradation_reports_none() {
        let conn = sample_connection(); // ThisHost never degrades
        let reason = scope_degradation(VerdictScope::ThisHost, Verdict::Deny, &conn);
        assert!(reason.is_none());
    }

    #[test]
    fn deny_verdict_any_host_degradation_is_surfaced_when_process_path_empty() {
        let mut conn = sample_connection();
        conn.process_path = String::new();
        let reason = scope_degradation(VerdictScope::AnyHost, Verdict::Deny, &conn);
        assert!(reason.is_some());
    }

    // -- FIX 1: rule name sanitization ------------------------------------

    #[test]
    fn rule_name_for_neutralizes_path_traversal() {
        let name = rule_name_for(Verdict::Allow, "../../../../etc/cron.d/x", 443);
        // The security property that actually matters: no `/` survives, so
        // `filepath.Join(l.Path, name + ".json")` can never treat the
        // sanitized name as more than one path component — there's no
        // separator left to walk up directories with. A leftover `..`
        // *substring* inside a single path component (no `/` around it) is
        // an ordinary, harmless filename character sequence on Linux; only
        // an exact `..` *component* (bounded by `/` or string edges after
        // path splitting) is special, and that can't happen here.
        assert!(
            !name.contains('/'),
            "rule name must never contain a path separator: {name}"
        );
    }

    #[test]
    fn rule_name_for_handles_empty_host() {
        let name = rule_name_for(Verdict::Allow, "", 443);
        assert!(!name.is_empty());
        assert!(!name.contains('/'));
    }

    #[test]
    fn rule_name_for_is_deterministic_for_the_same_input() {
        let a = rule_name_for(Verdict::Deny, "../../etc/passwd", 80);
        let b = rule_name_for(Verdict::Deny, "../../etc/passwd", 80);
        assert_eq!(a, b);
    }

    #[test]
    fn rule_name_for_distinct_hostile_hosts_do_not_collide() {
        let a = rule_name_for(Verdict::Deny, "../../etc/passwd", 80);
        let b = rule_name_for(Verdict::Deny, "../../etc/shadow", 80);
        assert_ne!(a, b, "distinct hostile inputs must not collide");
    }

    // -- MEDIUM-2 (issue #14 security review round 2): sanitization must be
    // injective, or a persisted Deny/Always rule for one host could be
    // silently overwritten by a later verdict for a colliding host. -------

    #[test]
    fn rule_name_for_distinguishes_hosts_that_collide_under_character_mapping_alone() {
        // "_dmarc.example.com", "/dmarc.example.com", and
        // "%dmarc.example.com" all sanitize to the identical string under
        // character-mapping alone (both `/` and `%` map to `_`, same as a
        // literal leading `_`) — the always-appended raw-input digest is
        // what actually distinguishes them.
        let a = rule_name_for(Verdict::Deny, "_dmarc.example.com", 443);
        let b = rule_name_for(Verdict::Deny, "/dmarc.example.com", 443);
        let c = rule_name_for(Verdict::Deny, "%dmarc.example.com", 443);
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn rule_name_for_distinguishes_dot_dash_variants_that_trim_identically() {
        // "evil.com", "evil.com.", and "evil.com--" all trim to the same
        // "evil.com" under leading/trailing `.`/`-` trimming alone.
        let a = rule_name_for(Verdict::Deny, "evil.com", 443);
        let b = rule_name_for(Verdict::Deny, "evil.com.", 443);
        let c = rule_name_for(Verdict::Deny, "evil.com--", 443);
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn any_host_scope_matches_on_process_path() {
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHost,
            &sample_connection(),
            0,
        );
        let op = rule.operator.unwrap();
        assert_eq!(op.r#type, "simple");
        assert_eq!(op.operand, "process.path");
        assert_eq!(op.data, "/usr/bin/curl");
    }

    #[test]
    fn any_host_scope_degrades_to_this_host_when_process_path_is_empty() {
        let mut conn = sample_connection();
        conn.process_path = String::new();
        let rule = verdict_to_rule(
            Verdict::Allow,
            VerdictDuration::Once,
            VerdictScope::AnyHost,
            &conn,
            0,
        );
        let op = rule.operator.unwrap();
        // Never an operator matching every process (which is what an empty
        // process.path data would functionally amount to).
        assert_ne!(op.operand, "process.path");
        assert_eq!(op.operand, "dest.host");
        assert_eq!(op.data, "github.com");
    }

    // -- sanitize_for_display / MEDIUM-1 (issue #14 security review round 2) --

    #[test]
    fn sanitize_for_display_neutralizes_markup_metacharacters() {
        let out = sanitize_for_display("<b>evil.example</b>", 64);
        assert!(!out.contains('<'));
        assert!(!out.contains('>'));
        assert_eq!(out, "&lt;b&gt;evil.example&lt;/b&gt;");
    }

    #[test]
    fn sanitize_for_display_strips_ansi_escape_sequences() {
        let out = sanitize_for_display("evil\x1b[31mred\x1b[0m.example", 64);
        assert!(!out.contains('\x1b'), "ESC byte must not survive: {out:?}");
        assert_eq!(out, "evil[31mred[0m.example");
    }

    #[test]
    fn sanitize_for_display_strips_newlines_and_control_chars() {
        let out = sanitize_for_display("evil.example\nInjected: fake line\r\n", 64);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        assert_eq!(out, "evil.exampleInjected: fake line");
    }

    #[test]
    fn sanitize_for_display_caps_length_by_characters_not_bytes() {
        let long_host = "a".repeat(500);
        let out = sanitize_for_display(&long_host, 64);
        assert!(
            out.chars().count() <= 65,
            "expected <= 64 chars plus the truncation marker, got {}",
            out.chars().count()
        );
        assert!(out.ends_with('…'));
    }

    #[test]
    fn sanitize_for_display_leaves_ordinary_hostnames_untouched() {
        assert_eq!(sanitize_for_display("github.com", 64), "github.com");
    }

    #[test]
    fn sanitize_for_display_strips_rtl_override() {
        // U+202E RIGHT-TO-LEFT OVERRIDE can make attacker-controlled bytes
        // *render* as a different, more trustworthy-looking string than
        // what they actually are — `char::is_control()` alone doesn't
        // catch it (it's category Cf, not Cc).
        let out = sanitize_for_display("evil\u{202E}moc.elpmaxe", 64);
        assert!(
            !out.contains('\u{202E}'),
            "RTL override must not survive: {out:?}"
        );
        assert_eq!(out, "evilmoc.elpmaxe");
    }

    #[test]
    fn sanitize_for_display_strips_zero_width_characters() {
        // U+200B ZERO WIDTH SPACE can hide characters entirely or defeat
        // naive substring-based review; also Cf, not Cc.
        let out = sanitize_for_display("ev\u{200B}il.example", 64);
        assert!(
            !out.contains('\u{200B}'),
            "zero-width space must not survive: {out:?}"
        );
        assert_eq!(out, "evil.example");
    }
}
