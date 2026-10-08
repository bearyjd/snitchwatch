# App-bound prompt scopes, second half (issue #44)

**Date:** 2026-10-07
**Issue:** #44, second half. The first half shipped in #50; its plan is
`2026-10-07-app-bound-prompt-scopes.md`.

**Blocked on:**
- #39 merging. Part A changes the `verdict_to_rule` contract its callers
  in `grpc_server.rs` rely on, and #39 rewrites that file.
- #48, for Part B only. Pre-#50 rules come from earlier sessions, so the
  Rules page cannot see them until #48 lands.

**Size:** S + S, as two independent PRs.

## Owner decision (settled)

1. **Refuse instead of degrading.** When `process_path` is empty, don't
   build a remembered rule; today the code degrades to a process-agnostic
   one. This changes the `grpc_server.rs` contract.
2. **Flag old rules on the Rules page.** Mark pre-#50 Snitchwatch host-only
   rules as "applies to all apps" and offer one-click delete. Leave
   migration to the user. No automatic rewrite.

## Citation convention

- `#39:` is PR #39 at `5c2b44a`.
- `main:` is `f65a2a4`, which includes #50. #39 does not touch
  `translator/verdict.rs`, `RulesPage.qml` or the Kirigami `rules/`
  module.

The pause-before-GUI fix shifts the lines inside `ask_rule` by about +5.

## Goal

- **Part A:** the bridge never creates a remembered rule that is not bound
  to a program. If the program is unknown:
  - the user's answer still applies to *this* connection, as a once-only
    reply;
  - the GUI is told why nothing was remembered.
- **Part B:** a user can see which saved Snitchwatch rules match every
  program, and delete them with one click.

## Out of scope

- Rewriting old rules automatically. The bridge cannot know which program
  an old host-only rule was meant for.
- An explicit "any app" scope option for hosts. It is a roadmap P0.1 idea;
  it belongs to P2.2.
- Flagging hand-written or stock-UI host-only rules. Those may be
  intentional.

## Findings

- **Rule construction** (`main:crates/snitchwatch-bridge/src/translator/verdict.rs`):
  - `verdict_to_rule` (`:193-221`) is infallible.
  - `bind_to_process` (`:598-615`) logs `warn!` and returns the bare host
    operator when `process_path` is empty.
  - `any_host_operator_checked` (`:542-556`) degrades `AnyHost` with an
    empty path to the host-only `this_host_operator` and reports
    `ScopeDegradation::ProcessPathUnavailable`. So `AnyHost` also produces
    a process-agnostic rule today.
- **Callers on #39:** `ask_rule`'s paused branch
  (`#39:grpc_server.rs:486-496`, `Allow`/`Once`/`ThisHost`) and its main
  path (`:597-607`).
- **Persistent-rule broadcast:** gated on `resolution.duration.remembers()`
  (`:609-619`). After #48 it also upserts the rules cache.
- **Old rule names:**
  - pre-#50: `snitchwatch-<allow|deny>-<host part>-<port>`
    (`git show 9fdb336:crates/snitchwatch-bridge/src/translator/verdict.rs:140-146`);
  - the host part has carried a `-<16 hex>` digest since issue #14's
    MEDIUM-2 fix, and older names have none;
  - #50 appends `-p<basename>-<16 hex>` (`main:verdict.rs:177-191`
    `rule_name_for`);
  - the daemon may add `-N` on a name clash (`vendor:daemon/rule/loader.go:332-342`).
- **Description:** every Snitchwatch prompt rule since M1.5 (`a81d571`)
  carries `description: "snitchwatch interactive verdict"`
  (`main:verdict.rs:213`). A GUI toggle keeps it, because `rule_from_wire`
  reads `description`.
- **Tests that pin today's fallback** (they must flip):
  - `host_scopes_keep_the_host_only_fallback_when_process_path_is_empty`
    (`main:verdict.rs:1336`);
  - `any_host_scope_degrades_to_this_host_when_process_path_is_empty`
    (`:1225`);
  - `deny_verdict_any_host_degradation_is_surfaced_when_process_path_empty`
    (`:1116`).
- **Existing Rules page support:**
  - the inspector's Delete has a confirmation (`main:RulesPage.qml:300-330`);
  - `RulesModel::delete_rule` emits `DeleteRule`;
  - after #48, the row disappears once the daemon confirms.

## Design

### Part A: refuse in the bridge (S)

1. **`verdict.rs`.**
   - `verdict_to_rule(...) -> Result<Rule, RuleRefusal>` returns
     `Err(RuleRefusal::ProcessPathUnknown)` when
     `duration.remembers() && conn.process_path.is_empty()`, for every
     scope.
   - Add an infallible `once_rule(verdict, scope, conn, now)` for the paused
     branch and the fallback. A `once` rule is never stored by the daemon
     (`loader.go:380-383` `addUserRule`), so a process-agnostic operator is
     harmless for it.
   - Downgrade `bind_to_process`'s `warn!` to `debug!` and update its doc:
     an empty path is now reachable only for once-only replies.
   - `RuleRefusal::describe()` returns a fixed `&'static str`, the same
     pattern as `ScopeDegradation::describe`, so no attacker-influenced text
     reaches the UI.
2. **`grpc_server.rs` `ask_rule`** (#39).
   - The paused branch uses `once_rule`.
   - On the main path, `Err(ProcessPathUnknown)`:
     - replies with `once_rule(resolution.verdict, resolution.scope, …)`;
     - skips the `UpdateRules` broadcast and the #48 cache upsert;
     - broadcasts the new `ServerMessage::VerdictNotRemembered { row_id, reason }`
       and the matching `Notice`.
   - The existing `DenyScopeNarrowed` logic is unchanged.
3. **Kirigami.**
   - Route `VerdictNotRemembered` to a passive notification on
     `ConnectionsPage` ("Applied this time only: the program could not be
     identified").
   - Prevention: in `PendingDecisionSheet.qml`, when the row has no process
     path, offer only "This time". Expose a `hasProcessPath` role if one
     isn't there already.
   - The sheet is modified by the honest-ui PR, so land this after it.

### Part B: flag old rules (S, after #48)

4. **Kirigami `rules/row_store.rs`.** Add `Rule::applies_to_all_apps()`,
   true when **all** of these hold:
   - `description == "snitchwatch interactive verdict"`;
   - the name starts with `snitchwatch-allow-` or `snitchwatch-deny-` and
     does **not** match `-p[A-Za-z0-9._-]*-[0-9a-f]{16}(-[0-9]+)?$`;
   - the operator contains no `process.path` operand at any depth;
   - the operator is host-only: a `simple` `dest.host` or `dest.ip`, or a
     `regexp` `dest.host`.

   The operator test is the authoritative one; the name and description
   only restrict flagging to Snitchwatch's own rules.
5. **`RulesModel` and `RulesPage.qml`.**
   - **Model:** expose an `appliesToAllApps` role and a
     `legacyHostOnlyCount` property.
   - **Flagged rows:** show a Warning chip "Applies to all apps" and a
     row-level **Delete** button that calls `deleteRule(name)` directly,
     with no confirmation. The rule is over-broad by construction, and the
     next connection prompts again.
   - **Page header:** when the count is non-zero, show an `InlineMessage`
     ("N rules saved by an earlier Snitchwatch apply to every app…") with
     a "Delete all" action, which does ask for confirmation.
   - **Plain text:** the honest-ui PR's `textFormat: Text.PlainText` rule
     applies to the new labels too.
6. **Release note.** Link the existing note in
   `2026-10-07-app-bound-prompt-scopes.md` ("Behavior changes and limits")
   from the release notes.

## Tests to write first

**Part A** (`verdict.rs`, `grpc_server/tests.rs`):

- **Refusal:** each scope (`ThisHost`, `AnyHostOnDomain`, `AnyHost`) with
  an empty path and each remembered duration (`FiveMinutes`,
  `UntilRestart`, `Always`) returns `Err(ProcessPathUnknown)`.
- **Once still works:** an empty path with `Once` returns `Ok`.
- **Unchanged naming:** `Once` keeps today's host-only operator and name
  (`rule_name_for` with `""` is byte-identical).
- **Flip the three pinned tests** listed in Findings.
- **`ask_rule`, remembered Allow, empty `process_path`:**
  - the reply has `duration == "once"` and the user's action;
  - no `UpdateRules` is sent;
  - exactly one `VerdictNotRemembered` is sent, for that `row_id`, with
    fixed text.
- **`ask_rule`, paused, empty `process_path`:** still auto-allows once.
- **Mock acceptance:** the reply passes
  `mock_opensnitchd::validate_rule_shape`.

**Part B** (Kirigami unit and QML):

- **`applies_to_all_apps` truth table:**
  - true for a pre-#50 name, our description and a `dest.host` operator;
  - true for a pre-digest name such as `snitchwatch-allow-github.com-443`;
  - true for a `-2` suffix;
  - true for a `regexp dest.host`;
  - false for a #50 `list` rule;
  - false for a pre-#50 `AnyHost` rule (a `process.path` operator);
  - false for a hand-written `dest.host` rule with another description;
  - false for a `z00-blocklist:` rule.
- **QML:** a flagged row shows the chip, and Delete emits `DeleteRule` with
  that name.

## Verification

Run at low priority:

- `cargo test -p snitchwatch-bridge`
- `cargo clippy -p snitchwatch-bridge --all-targets -- -D warnings`
- `cargo test -p snitchwatch-kirigami rules` with
  `QT_QPA_PLATFORM=offscreen` and `QT_QUICK_CONTROLS_STYLE=Basic`

Manual VM check for Part B:

1. Put a pre-#50-shaped rule JSON into `/etc/opensnitchd/rules/`.
2. Restart the bridge: the rule appears flagged.
3. One click removes it, from the page and from disk.

## Risks and open questions

- **What to reply when refusing.** The plan replies once-only with the
  user's verdict (fail-safe: no persistent over-broad rule, and the user's
  intent holds for this connection). The alternative is `Unavailable`, so
  the daemon applies its default action. That ignores the user's click. The
  plan assumes once-only; owner's call if not.
- **Reachability is low** with the shipped `InterceptUnknown: false` (#44
  comment), so Part A is mostly a contract hardening.
- **False negatives in flagging.** A pre-#50 rule whose description a user
  edited in another tool won't be flagged. That is acceptable: such a rule
  was deliberately changed.
- **File-conflict hot spots:**
  - `grpc_server.rs` `ask_rule`, with #47's paused branch and #48's cache
    upsert; land after both, or rebase;
  - `PendingDecisionSheet.qml`/`RulesPage.qml`, with the honest-ui PR.
