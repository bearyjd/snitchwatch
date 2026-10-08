# App-bound prompt scopes (issue #44, first half)

**Date:** 2026-10-07
**Issue:** #44 (roadmap P0.1 in `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`)
**Severity:** HIGH — "This host" / "Any host on this domain" answers with a
remembered duration create rules that match every process.

## Goal

When the connection's `process_path` is known, the `ThisHost` and
`AnyHostOnDomain` scopes produce a rule that matches **this program AND
the host/domain**, and rules for different programs never share a name.

## Out of scope (second half of #44, after draft PR #39 merges)

- Refusing to build a rule when `process_path` is empty (today it degrades
  to a process-agnostic host rule). That changes the contract
  `grpc_server.rs` relies on, and #39 rewrites `grpc_server.rs`.
- Any change to `AnyHost` (already `process.path`-only) or its
  empty-path fallback.
- Migrating host-only rules users already have (release-note guidance).

## Design

1. **Operator.** In `translator/verdict.rs`, `build_operator_checked` wraps
   the existing host/IP/domain operator for `ThisHost` and
   `AnyHostOnDomain` in an opensnitchd `list` operator when
   `conn.process_path` is non-empty:
   `{type: "list", operand: "list", data: "", list: [process.path simple, <host op>]}`.
   opensnitchd ANDs list members (`daemon/rule/operator.go` `listMatch`),
   compiles each member on load (`loader.go:413-420`), and copies members
   from the proto in `Deserialize` (`rule.go:113-127`). `process.path`
   first so its exact compare short-circuits, and **case-sensitive**
   (`sensitive: true`): a non-sensitive simple operator compares with
   Unicode case folding (`strings.EqualFold`). The same applies to the
   `AnyHost` scope's existing `process.path` operator (added during
   review). Degradation reporting (`scope_degradation`) is unchanged — it
   reads the second tuple element. The bridge's wire format
   (`operator_to_wire` / `operator_from_wire`) and the Kirigami row store
   and simulator already handle list operators.
2. **Rule name.** `rule_name_for(verdict, host, port)` gains a
   `process_path: &str` argument. When non-empty it appends
   `-p<sanitized basename>-<16 hex of SHA-256(raw path)>`; when empty the
   name is byte-identical to today's. Without this, two programs' rules for
   the same verdict and `host:port` would share a name: the daemon stores
   the second as `<name>-2` (`setUniqueName`, `loader.go:332-341`) while
   the bridge and Rules page still call it `<name>`, so a later toggle or
   delete edits the other program's rule. `AnyHost` rule names gain the
   same component.
3. **Call sites.** `verdict_to_rule` passes `conn.process_path`;
   `ConnectionCache::resolve` (`cache/connections.rs`) passes the row's
   `process_path` so `matched_rule` keeps matching the real rule name.
   #39 adds no calls to either function and its nearest
   `cache/connections.rs` hunk ends above this call site.

## Behavior changes and limits (for release notes)

- Host-only rules users already saved keep matching every program: new
  names never replace them. Users should delete and re-answer them.
- A "This host" **Deny** now blocks only the asking program, not every
  program reaching that host.
- Binding is to the executable *path* the daemon reports, not a verified
  program: it's the path in the process's own mount namespace (so a
  bind-mount or another Flatpak at the same `/app/...` path inherits the
  rule), interpreters share their interpreter's path, and when
  `/proc/<pid>/exe` is unreadable the daemon falls back to `comm`/argv[0]
  or a placeholder. AppImages run from a random `/tmp/.mount_*` path, so
  their saved rules stop matching after a relaunch.
- The mock daemon's rule validator now recurses into `list` members and
  rejects an empty list (which opensnitchd's `listMatch` would treat as
  matching everything).

## Tests (write first)

- `ThisHost` with a process → list of `process.path == path` AND
  `dest.host == host`; bare-IP connection → AND `dest.ip`.
- `AnyHostOnDomain` with a process → list with the domain regexp;
  a degraded domain answer is still process-bound.
- Empty `process_path` → unchanged host-only fallback (documents the
  deferred second half).
- `rule_name_for`: same verdict/host/port, different programs → different
  names; empty process → today's name exactly; process basename
  neutralized for path characters.
- `rule_name_for_matches_verdict_to_rule_name` and the cache's
  `matched_rule` test updated for the process argument.
- Existing domain-wildcard tests keep asserting the host member via a
  strict helper that requires the process-bound list shape.

## Verification

`cargo test -p snitchwatch-bridge` and
`cargo clippy -p snitchwatch-bridge --all-targets -- -D warnings`, run at
low priority (another session runs timing-sensitive VM tests on this
host); CI runs the full workspace.
