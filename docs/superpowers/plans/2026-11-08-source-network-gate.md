# Plan: refuse `network` + `source.network` when the daemon is >= 1.9 (version-gated)

Date: 2026-11-08. Status: DRAFT, awaiting owner confirm. Owner decision (2026-11-08): **version-gated**
(keep 1.8.x behaviour, refuse/warn for >= 1.9). Cause: the rule-engine review of upstream master
a1353848 (daemon reports 1.9.0 in `core/version.go`) found it REFUSES to compile a rule of type
`network` with operand `source.network` (`daemon/rule/operator.go:186-188`; v1.8.0 accepted it);
such a rule from disk is skipped with a warning at daemon start, and `CHANGE_RULE` gets ERROR
"(2) error compiling rule". Our bridge, editor, import schema and mock all accept it today.
Claims not read from a file are marked **UNVERIFIED**. Mapped read-only against main 3a3b38a6.

## Facts
- The shipped daemon (bluebuild RPM, and the tower unless it moves) is v1.8.0: `source.network`
  works there, so it must keep working. Only a >= 1.9 daemon refuses it.
- The bridge never stores the daemon version. It arrives as strings in `ClientConfig.version`
  (Subscribe; `grpc_server.rs:503` logs and drops it) and `Statistics.daemon_version` (Ping; sanitized to
  32 chars and broadcast, `grpc_server.rs:335`, not stored). Subscribe is the reliable one (Ping is skipped
  when idle). Mock/test values are not semver: `"mock-1.8.0"`, `"mock-1.6.0"`, `"1.8.0"`.
- Validation is context-free: `rule_policy.rs:129` `NETWORK_OPERANDS`, `validate_leaf :270-303`,
  `validate_operator :176`; callers `rule_wire.rs:109` (GUI create/edit), `daemon_commands.rs:541`
  (`send`, defence in depth), `rule_io.rs:236,266` + `rule_io/preview.rs:159` (import),
  `profiles/materializer.rs:102` + `daemon_commands/profile_command.rs:87` (profiles).
  `read_only_reason (:190)` calls `validate_operator`, so a gate inside it would make existing rules read-only.
- Curated rules and blocklists never use `source.network`: no change.
- Tests that lock acceptance: `rule_policy/tests.rs:158`, `profile_tests.rs:303,375`, mock
  `rule_validation.rs:73,87,153-162`, `mock_opensnitchd/src/tests/rule_shape.rs:175`.

## Decisions I am proposing (confirm or change)
1. **Existing `source.network` rules stay editable and are not made read-only**; the bridge only WARNS about
   them against a >= 1.9 daemon (it is **UNVERIFIED** how much the daemon drops vs keeps on load; the review
   says a stored rule is skipped at start). Refusal applies to NEW/changed rules and to import/profile apply.
2. **Unknown version = today's 1.8.x behaviour** (accept). The daemon's own refusal is the backstop and the
   shipped daemon is 1.8.x.
3. **Two PRs.** PR A: bridge + mock (the part that protects users). PR B: GUI (hide the operand and show
   the capability), which needs a new server message/capability and Kirigami editor changes. PR A already
   gives a clear error even while the editor still offers the operand.

## PR A (bridge + mock)
1. `daemon_config.rs`: `DaemonVersion { major, minor }` with a lenient parser (first `\d+\.\d+`; handles
   `"mock-1.8.0"`, `"v1.9.0-rc1"`, empty => None). A `SharedDaemonVersion` handle set at Subscribe
   (`grpc_server.rs:505` from `cfg.version`) and refreshed from stats (`:335`) if Subscribe gave none; global,
   last write wins (self-corrects on the next Subscribe).
2. `rule_policy.rs`: a `PolicyContext { refuse_source_network: bool }` threaded through `validate_leaf`,
   `validate_operator`, `validate_user_rule`, `check_wire_rule`, with a no-context wrapper meaning 1.8
   behaviour so untouched call sites and tests keep compiling. Message: "opensnitchd 1.9 and newer only
   accepts dest.network for a network condition; use dest.network instead." Fix nothing else.
3. Pass the context from `rule_wire` (edit path), `daemon_commands::send`, `rule_io` import check/preview/apply
   (apply re-checks: preview and apply may see different versions), and the profile materializer/activation.
4. Mock daemon: a configurable `version` (default `"1.8.0"`), `rule_validation.rs` refuses `source.network`
   for >= 1.9, and `lists.ips` added to `IP_OPERANDS` (the review: otherwise the mock accepts simple +
   `lists.ips`, which panics master).
5. Tests (failing-first): parser table; policy with/without context; mock at 1.8 vs 1.9; a protocol test
   (`tests/bridge_protocol_test`) that a 1.9 mock rejects and a 1.8 mock accepts the rule through the bridge;
   import preview/apply across a version change; existing acceptance tests keep passing unchanged.
6. Widen `list_operand_names_match_the_vendored_daemon` (`history/rule_identity_tests.rs:327-345`) so the
   next vendor upgrade fails loudly on the new `range` type and the `dest.network`-only rule instead of
   passing on stale facts (it currently reads `operator.go` only for `lists.*` operand names).

## PR B (GUI, after A)
A bridge capability/message (`daemonRefusesSourceNetwork`, only when the version is known and >= 1.9;
`bridge_capabilities::advertised()` is static today so it needs a dynamic hook or a post-Subscribe message);
Kirigami `editor::operands(hide_source_network)` and `editor::check` read it (`editor.rs:94,166-172,199,431`);
the simulator keeps evaluating `source.network` so existing rules still simulate (`operator.rs:348`) but
flags `network` + `source.network` as "daemon 1.9 refuses to load" and regexp + network operands as
"daemon panics" (`operator.rs:542` says "definitely doesn't match" today). The import JSON schema keeps
`source.network` (validity depends on the daemon; only the bridge can judge).

## Risks / races
Validated before the first Subscribe (None => 1.8 behaviour, a 1.9 daemon could briefly be sent it; the daemon
refusal is the backstop); daemon upgrade mid-session (self-corrects); a proxy/fork can report any version
string; the GUI flag arrives asynchronously so an open draft must tolerate it flipping.

## Process
Branch `feat/source-network-gate`, executor builds, I run fmt/clippy/tests on a throwaway HOME, independent
code + security-ish review on the exact head, 6/6 CI, pinned merge. The pin PR (`chore/pin-opensnitch-master`,
worktree /var/home/user/w2, uncommitted) waits for PR A so the pin never lands ahead of the guard.
