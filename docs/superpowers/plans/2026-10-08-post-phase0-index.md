# Post–Phase 0 plans: index, order and conflict map

**Date:** 2026-10-08
**Scope:**
- the two owner-requested items queued after Phase 0 (overnight board
  items 9a and 9b);
- the first three Phase 2 items from
  `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md` §3,
  least ambiguous first.

**Baseline for every plan:** `main` @ `4b3ba52`. It includes:
- #48 (show every daemon rule, PR #60):
  - `cache/rules.rs` `RulesCache`/`RulesSync`;
  - `daemon_commands.rs` `DaemonCommands`, with a send-point allowlist
    `{ChangeRule, DeleteRule}` plus rule-name validation;
  - `rule_wire.rs`, which owns `rule_to_wire`/`rule_from_wire`; its
    `rule_to_wire` emits `displayName` and `readOnlyReason`;
- #47 (timed pause, PR #59);
- the rule-name traversal fix (PR #57);
- #49 (PR #56);
- the honest-UI PR (#53).

**In flight, and reused by these plans:** a security PR (branch
`fix/rule-operator-validation`) creates
`crates/snitchwatch-bridge/src/rule_policy.rs`.
- **What it does:** `validate_operator` checks type↔operand **pairing**,
  `list` shape and nesting, and refuses `lists.*`. It is applied in
  `rule_from_wire` for GUI-sourced rules.
- **These plans don't define it; they call it.** P2.7 and P2.1 only add
  profile-level checks on top. Re-check its exact names when it merges.

**Go by function names, not line numbers.**

## Daemon constraint (all plans)

bazzite-tower's system-variant daemon (PR #81) accepts **only**
`CHANGE_RULE` and `DELETE_RULE` notifications from the UI. It refuses
`lists.*` operands, nested ones included, until #45 PR B delivers the
list-path contract. #48's `DaemonCommands` enforces the same allowlist on
the bridge side (`ALLOWED_ACTIONS`).

| Plan | What reaches the daemon |
|---|---|
| Inline Deny | an `AskRule` **reply** (not a notification); undo is `DELETE_RULE` |
| Prompt-slot A/B | nothing new |
| Prompt-slot C | `AskRule` replies, or `Status::unavailable` |
| Prompt-slot D | `CHANGE_RULE` to install, `DELETE_RULE` to reconcile (only names under the reserved `snitchwatch-default-` prefix) |
| Prompt-slot E | daemon internals only (for tower to choose); no new UI action |
| Import/export | `CHANGE_RULE`, one rule per notification; never `DELETE_RULE` |
| Insights | nothing (read-only) |
| Editor | `CHANGE_RULE`; rename adds one `DELETE_RULE` |

No plan needs `CHANGE_CONFIG`, `TASK_START` or `RELOAD_FW_RULES`.
- **Prompt-slot** can't change `DefaultAction`, so it works around it.
- **Editor and import** refuse hash operands rather than turn on
  `EnableChecksums`.

## Plans

| Item | Plan | Size | Depends on | Owner questions |
|---|---|---|---|---|
| 9a inline Deny until restart | `2026-10-08-inline-deny-until-restart.md` | S | **#44 Part A** (bridge refusal of non-absolute paths, `bindableProcessPath` role, `&str` predicate) | none (D1–D3 decided) |
| 9b prompt slot | `2026-10-08-prompt-slot-ux.md` | M (A, B: S each; C, D: M) | A/B: #44 Part A (shared `Notice` sites) and the inline-Deny plan (shared Deny semantics). C: S1/S2 answered. D: the security PR's `rule_policy.rs`, the capture spike, S3 | S1, S2, S3, S4 (daemon option), S5 |
| P2.7 import/export | `2026-10-08-rule-import-export.md` | S–M | the security PR's `rule_policy.rs` | none (X1–X4 decided) |
| P2.6 insights | `2026-10-08-rule-insights.md` | M (3 PRs) | none for any part now that #48 is merged. N1 persistence needs #45 PR A's state dir | N1 (N2 decided) |
| P2.1 rule editor | `2026-10-08-rule-editor.md` | M–L | the security PR's `rule_policy.rs`; P2.7's profile layer if it lands first | E2 (E1, E3 decided) |

Existing plans this builds on:
- `2026-10-07-app-bound-prompt-scopes-part2.md` (#44 A/B);
- `2026-10-07-show-all-daemon-rules.md` (#48);
- `2026-10-07-blocklist-enforcement.md` (#45);
- `2026-10-07-profile-enforcement.md` (#46 Part 2 waits for the editor).

## Recommended order

#48 has merged (`4b3ba52`), so the "wait for #48" step is done.

1. **#44 Part A** (existing plan). It is a hard prerequisite for inline
   Deny: without it, an `until restart` deny with an empty or placeholder
   path blocks every app, or binds to a forgeable path.
2. **Inline Deny** (S), right after #44 Part A. It shares
   `ConnectionsPage.qml` and the `bindableProcessPath` role.
   - **Ask tower for an r6/r7 VM run** with the acceptance check quoted in
     that plan, **on a `DefaultAction: deny` image** (see that plan's
     Verification).
3. **Prompt-slot A + B** (visibility, notification actions). Land after
   #44 Part A, since both add `Notice` variants. Meanwhile:
   - put **S1/S2/S5** to the owner;
   - start the **capture spike** for D on tower's VM, once tower's
     rollout-gate timing work is done (roadmap §6 item 5);
   - send tower the **E options** (S4).
4. **The security PR's `rule_policy.rs`** (in flight). It gates P2.7,
   P2.1 and prompt-slot D.
5. **P2.7 import/export.** It adds the `PolicyProfile::Import` layer
   (`validate_user_rule`) on top of `validate_operator`, plus
   `rule_io.rs`.
6. **P2.6 insights**, in three PRs. Part 3 (simulator) can run any time in
   parallel; Part 2 follows Part 1.
7. **P2.1 editor**, last of the three. It adds the `Editor` profile and
   `request_id`/`RuleCommandResult`. **#46 Part 2** follows it.
8. **Prompt-slot C** once S1/S2 are answered (it edits `ask_rule`, so
   rebase on #44 Part A). **Prompt-slot D** once `rule_policy.rs` exists
   and S3 is answered.

#45 PR B can land any time now. It doesn't lift the `lists.*` refusal in
the editor or import: list rules stay authored by #45's materializer only.

## Shared modules: who creates them

| Module | Created by | Also used by |
|---|---|---|
| `crates/snitchwatch-bridge/src/rule_policy.rs` `validate_operator` (type↔operand pairing, `list` shape/nesting, `lists.*` refusal) | **the security PR** (`fix/rule-operator-validation`), applied in `rule_from_wire` | P2.7 (`validate_user_rule`, `Import` profile), P2.1 (`Editor` profile), prompt-slot D, #46 Part 2, Kirigami editor (via the `snitchwatch-bridge` dependency) |
| `crates/snitchwatch-bridge/src/daemon_config.rs` (`DefaultAction`, `Stats.MaxEvents`, `Rules.EnableChecksums` from `ClientConfig.config`) | whichever of **prompt-slot C / P2.6 Part 1** lands first | the editor and simulator (hash warnings) |
| `RulesCache` revision counter (bumped in every mutating `RulesCache` method, so `prune_expired_rules_every`'s direct prune counts too) | P2.7 | P2.1 (stale edit checks, optional) |
| `bindable_process_path(&Connection)` and its `&str` form `is_bindable_process_path` (bridge), plus the `bindableProcessPath` role (Kirigami) | #44 Part A | inline Deny (`&str` form), prompt-slot C (P-c fallback), editor prefill |
| `applies_to_all_apps` predicate | #44 Part B (Kirigami `rules/row_store.rs`) | P2.7 preview flag, P2.1 warning (generalised to "no `process.*` operand at any depth") |
| `within_limits` / `parse_duration_secs` (#48 `cache/rules.rs`, made `pub(crate)`) | P2.7 / P2.1 | `rule_policy.rs` |

## File-conflict map (by function)

Abbreviations:
- **44A** = #44 Part A
- **ID** = inline Deny
- **PS** = prompt-slot
- **IO** = P2.7 import/export
- **IN** = P2.6 insights
- **ED** = P2.1 editor

| File (function) | 44A | ID | PS | IO | IN | ED |
|---|---|---|---|---|---|---|
| Kirigami `ConnectionsPage.qml` (`submitInlineVerdict`, `submitBatchVerdict`, row buttons) | ✓ | ✓ | ✓ Decide later, countdown | | | |
| Kirigami `ConnectionsPage.qml` inspector | | | | | ✓ "Simulate this connection" | ✓ "Create rule from this connection" |
| `connections_model.rs` roles / `connections/row_store.rs` | ✓ `bindableProcessPath` | ✓ `inline_duration_for` | ✓ deadline/deferred roles | | | |
| `pending_decision.rs`, `PendingDecisionSheet.qml` | ✓ | ✓ (D2/D3) | | | | |
| `main.qml` `pendingExposureBanner` | | | ✓ | | | |
| `notice.rs`, tauri `notifier.rs`, kirigami `notifier.rs` / `notification_controller.rs` | ✓ `VerdictNotRemembered` | | ✓ `PromptSlotSummary`, actions | | | |
| `grpc_server.rs` `ask_rule` | ✓ | | ✓ slot hold, timeout arm | | | |
| `grpc_server.rs` `ping` | | | ✓ `rule_misses` | | ✓ `RuleHits::record` | |
| `grpc_server.rs` `subscribe` | | | ✓ `daemon_config` | | ✓ `daemon_config` | |
| bridge-cli `lib.rs` `run_with_incoming` inbound pump, rule-effect arm | | | ✓ D reconcile | ✓ new arms, import task | | ✓ policy, `request_id`, rename |
| bridge-cli `lib.rs` `SnapshotRequested` arm | | | ✓ `PromptSlot` | | ✓ `RuleHits` | |
| `ws_messages.rs` | ✓ | | ✓ | ✓ | ✓ | ✓ |
| `translator/upstream.rs` `apply` / `UpstreamEffect` | | | | ✓ | | ✓ |
| `translator/rule_notification.rs` | | | | | | ✓ |
| `translator/connection.rs` `event_to_row` | | | (E3 only) | | | |
| `cache/rules.rs` (`within_limits` visibility, `RulesCache` revision) | | | | ✓ | ✓ prune → hits | ✓ |
| new: `prompt_slot.rs`, `daemon_config.rs`, `rule_io.rs`, `cache/rule_hits.rs` | | | ✓ / ✓ | ✓ | ✓ | |
| `rule_policy.rs` (created by the security PR; profile layers added) | | | ✓ D uses it | ✓ `Import` | | ✓ `Editor` |
| `ws_server.rs` `pump_authenticated` (size check before `from_str::<ClientMessage>`) | | | | ✓ | | |
| Kirigami `rules/simulator.rs` | | | | | ✓ | ✓ "Test this rule" |
| Kirigami `rules/row_store.rs`, `rules_model.rs` | (#44B) | | | ✓ | ✓ | ✓ |
| `RulesPage.qml` (header actions, columns, row actions) | (#44B) | | | ✓ | ✓ | ✓ |
| `tests/bridge_protocol_test.rs`, `tests/mock_opensnitchd` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

**Hot spots:**
- `ws_messages.rs` takes additive variants from every plan;
- `ask_rule` (44A, PS);
- `ping` (PS, IN);
- the pump's rule-effect arm (IO, ED, PS-D);
- `RulesPage.qml` (#44B, IO, IN, ED).

Land in the order above and rebase. Expect trivial conflicts only, since
every protocol change is an additive variant or an additive optional
field.

## OWNER QUESTIONS (roll-up)

Only questions that need the owner remain. Everything else is decided in
the plans, with rationale (list below).

**Open**
- **S1** (prompt slot): silent auto-answer: on/off, the timeout, and the
  reply. **Recommendation:** on, 30 s, daemon default (P-a).
- **S2** (prompt slot): "Decide later" semantics.
  **Recommendation:** block the program for 5 min (P-c).
- **S3** (prompt slot): curated defaults: opt-in or on by default, the
  list, `kioworker`/Steam scope, per-user path regexps.
  **Recommendation:** opt-in; `/usr` only; host-constrained; no regexps
  in v1.
- **S4** (prompt slot, daemon-option part only): may we ask tower for E2
  ("drop while busy")? It changes daemon behaviour under both
  `DefaultAction` values. Asking tower for E3 (visibility only) and
  contributing E1 upstream need no owner decision.
- **E2** (editor): `process.path` patterns in hand-written rules.
  **Recommendation:** an exact match must be absolute; a pattern is allowed
  with a warning.
- *Borderline* **N1** (insights): persist hit counts before P3.1.
  **Recommendation:** yes.
- *Borderline* **S5** (prompt slot): answer from desktop notifications.
  **Recommendation:** Allow-once and Deny only.

**Decided in the plans (no owner input needed)**
- **D1 = yes.** "Deny all (N)" gets the until-restart app-bound rule. It
  follows from the owner's inline-Deny decision: same button family, same
  retransmit flaw.
- **D2 = yes.** Add the hint under the sheet's "This time" Deny.
- **D3 = yes.** "Until quit" → "Until firewall restarts". This follows the
  honest-UI rule: the label must describe what the daemon does.
- **X1:** export `always` + `until restart` user rules, with counts of
  what was left out.
- **X2:** replace on a same name, ticked by default, except loosening ones.
- **X3:** no "replace all" in v1.
- **X4:** on-disk rule files in v1.1.
- **N2:** 14 days, shown only if N1 = yes.
- **E1:** allow rename as a two-step.
- **E3:** `precedence`/`nolog` in v1, under "Advanced".

## Verification constraint (all plans)

Another session runs timing-sensitive VM tests on this host:
- run cargo with `nice -n 19`, one crate at a time, with
  `CARGO_TARGET_DIR` at the `/var/home/...` spelling;
- never point a test at the live `opensnitchd`;
- any test that starts a bridge isolates `XDG_RUNTIME_DIR` (and, after
  #45 PR A, the state directory) through `Command::env`, never
  `std::env::set_var`.
