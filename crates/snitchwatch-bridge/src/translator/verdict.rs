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
/// traversal), trims leading/trailing `.`/`-`, and — so distinct hostile
/// inputs never collide onto the same rule name — falls back to a stable
/// hash of the raw bytes when the sanitized result would be empty or
/// unreasonably long.
fn sanitize_host_for_rule_name(raw: &str) -> String {
    const MAX_LEN: usize = 64;

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

    if trimmed.is_empty() || trimmed.len() > MAX_LEN {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        raw.hash(&mut hasher);
        format!("host-{:016x}", hasher.finish())
    } else {
        trimmed.to_string()
    }
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

    let (operator, _degradation_reason) = build_operator_checked(scope, conn);

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

/// Non-`None` exactly when `scope` couldn't be honored as requested and
/// [`build_operator_checked`] silently degraded to an exact-host/process-path
/// fallback. Only meaningful for `Deny` — see issue #14 security review
/// FIX 2: a narrower `Allow` is fail-safe (the user still only gets what they
/// explicitly asked for, just less of it, so [`verdict_to_rule`] applies it
/// silently), but a narrower `Deny` silently *under-blocks* relative to what
/// the pending-decision dialog offered. The caller (the gRPC `ask_rule`
/// handler) uses this to tell the client the block was scoped down, rather
/// than let the UI imply the wider block succeeded.
pub fn scope_degradation_reason(
    scope: VerdictScope,
    verdict: Verdict,
    conn: &Connection,
) -> Option<String> {
    if verdict != Verdict::Deny {
        return None;
    }
    build_operator_checked(scope, conn).1
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
/// operator — see [`scope_degradation_reason`].
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
fn any_host_on_domain_operator_checked(conn: &Connection) -> (Operator, Option<String>) {
    let host = conn.dst_host.as_str();

    if host.is_empty() {
        return (
            this_host_operator(conn),
            Some("no destination host recorded for this connection".to_string()),
        );
    }
    if host.parse::<IpAddr>().is_ok() {
        return (
            this_host_operator(conn),
            Some("destination is a bare IP address, not a hostname".to_string()),
        );
    }
    if !is_valid_hostname_shape(host) {
        return (
            this_host_operator(conn),
            Some(format!("hostname `{host}` failed shape validation")),
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
                Some(format!("hostname `{host}` failed DNS-name parsing")),
            )
        }
    };
    // `.prefix()` is `None` exactly when `host` IS its own registrable
    // domain (eTLD+1) — nothing beyond the registrable owner's label to
    // safely wildcard away. This is what correctly rejects `example.com`
    // (registrable domain = itself), `shop.co.uk` (`co.uk` is the eTLD,
    // `shop.co.uk` is the eTLD+1 = itself), and `user.github.io`
    // (`github.io` is the eTLD, `user.github.io` is the eTLD+1 = itself).
    if parsed.prefix().is_none() {
        let root = parsed.root().unwrap_or(host);
        return (
            this_host_operator(conn),
            Some(format!(
                "host `{host}` is already at its registrable domain `{root}` — \
                 wildcarding would match every host under a public suffix"
            )),
        );
    }

    // Safe to wildcard: host has at least one label beyond its registrable
    // domain, so dropping only the leftmost label never reaches (or goes
    // above) the registrable owner's boundary.
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
fn any_host_operator_checked(conn: &Connection) -> (Operator, Option<String>) {
    if conn.process_path.is_empty() {
        tracing::warn!(
            dst_host = %conn.dst_host,
            dst_ip = %conn.dst_ip,
            "AnyHost verdict scope requested with an empty process_path; \
             degrading to ThisHost rather than emit an unscoped rule"
        );
        return (
            this_host_operator(conn),
            Some("process path unavailable for this connection".to_string()),
        );
    }
    (
        simple_operator("process.path", conn.process_path.as_str()),
        None,
    )
}

fn build_operator_checked(scope: VerdictScope, conn: &Connection) -> (Operator, Option<String>) {
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

    // -- FIX 2: Deny must never silently narrow ---------------------------

    #[test]
    fn allow_verdict_degradation_is_silent() {
        let mut conn = sample_connection();
        conn.dst_host = "shop.co.uk".to_string(); // would degrade
        let reason = scope_degradation_reason(VerdictScope::AnyHostOnDomain, Verdict::Allow, &conn);
        assert!(
            reason.is_none(),
            "Allow degradation must not be surfaced (fail-safe narrowing)"
        );
    }

    #[test]
    fn deny_verdict_degradation_is_surfaced() {
        let mut conn = sample_connection();
        conn.dst_host = "shop.co.uk".to_string(); // would degrade
        let reason = scope_degradation_reason(VerdictScope::AnyHostOnDomain, Verdict::Deny, &conn);
        assert!(
            reason.is_some(),
            "Deny degradation must be surfaced — silent under-blocking is a defect"
        );
    }

    #[test]
    fn deny_verdict_with_no_degradation_reports_none() {
        let conn = sample_connection(); // ThisHost never degrades
        let reason = scope_degradation_reason(VerdictScope::ThisHost, Verdict::Deny, &conn);
        assert!(reason.is_none());
    }

    #[test]
    fn deny_verdict_any_host_degradation_is_surfaced_when_process_path_empty() {
        let mut conn = sample_connection();
        conn.process_path = String::new();
        let reason = scope_degradation_reason(VerdictScope::AnyHost, Verdict::Deny, &conn);
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
}
