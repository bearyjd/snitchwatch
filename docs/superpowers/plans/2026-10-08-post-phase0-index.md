# Post–Phase 0 plans: index, order and conflict map

**Date:** 2026-10-08
**Scope:**
- the two owner-requested items queued after Phase 0 (overnight board
  items 9a and 9b);
- the first three Phase 2 items from
  `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md` §3,
  least ambiguous first.

**Baseline for every plan:** `main` @ `d9d1bfe`. It includes #47 (timed
pause, PR #59), the rule-name traversal fix (PR #57), #49 (PR #56) and the
honest-UI PR (#53).

**#48 (show every daemon rule) is about to merge.** The plans cite it as
branch `fix/48-show-all-rules` @ `ebdd21d`, a squashed rebase onto
`d9d1bfe`. **Re-check every `#48:` name after it merges.** In particular,
`rule_to_wire`/`rule_from_wire` move from `grpc_server.rs` to
`rule_wire.rs`.

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
| Prompt-slot D | `CHANGE_RULE` to install, `DELETE_RULE` to reconcile |
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
| 9a inline Deny until restart | `2026-10-08-inline-deny-until-restart.md` | S | **#44 Part A** (bridge refusal of non-absolute paths, `bindableProcessPath` role) | D1–D3 |
| 9b prompt slot | `2026-10-08-prompt-slot-ux.md` | M (A, B: S each; C, D: M) | A/B: #44 Part A (shared `Notice` sites) and the inline-Deny plan (shared Deny semantics). C: S1/S2 answered. D: #48, `rule_policy.rs`, the capture spike, S3 | S1–S5 |
| P2.7 import/export | `2026-10-08-rule-import-export.md` | S–M | #48 | X1–X4 |
| P2.6 insights | `2026-10-08-rule-insights.md` | M (3 PRs) | Parts 1–2: #48. Part 3: none (uses #48's `Rule.precedence` once merged). N1 persistence needs #45 PR A's state dir | N1–N2 |
| P2.1 rule editor | `2026-10-08-rule-editor.md` | M–L | #48, `rule_policy.rs` | E1–E3 |

Existing plans this builds on:
- `2026-10-07-app-bound-prompt-scopes-part2.md` (#44 A/B);
- `2026-10-07-show-all-daemon-rules.md` (#48);
- `2026-10-07-blocklist-enforcement.md` (#45);
- `2026-10-07-profile-enforcement.md` (#46 Part 2 waits for the editor).

## Recommended order

1. **#44 Part A** (existing plan). It is a hard prerequisite for inline
   Deny: without it, an `until restart` deny with an empty or placeholder
   path blocks every app, or binds to a forgeable path.
2. **Inline Deny** (S), right after #44 Part A. It shares
   `ConnectionsPage.qml` and the `bindableProcessPath` role.
   - **Ask tower for an r6/r7 VM run** with the acceptance check quoted in
     that plan.
3. **Prompt-slot A + B** (visibility, notification actions). They don't
   need #48. Land after #44 Part A, since both add `Notice` variants.
   Meanwhile:
   - put **S1/S2** to the owner;
   - start the **capture spike** for D on tower's VM, once tower's
     rollout-gate timing work is done (roadmap §6 item 5);
   - send tower the **E options** (S4).
4. **#48 merges.** That gates everything below.
5. **P2.7 import/export.** It **creates `rule_policy.rs`** (see Shared
   modules) and `rule_io.rs`.
6. **P2.6 insights**, in three PRs:
   - Part 3 (simulator) can run any time in parallel, being Kirigami-only;
   - Part 1 (hit counts) after #48;
   - Part 2 after Part 1.
7. **P2.1 editor**, last of the three. It extends `rule_policy.rs` with
   the `Editor` profile and adds `request_id`/`RuleCommandResult`. **#46
   Part 2** follows it.
8. **Prompt-slot C** once S1/S2 are answered (it edits `ask_rule`, so
   rebase on #44 Part A). **Prompt-slot D** after the editor's policy
   module exists and S3 is answered.

#45 PR B can land anywhere after #48. It doesn't lift the `lists.*`
refusal in the editor or import: list rules stay authored by #45's
materializer only.

## Shared modules: who creates them

| Module | Created by | Also used by |
|---|---|---|
| `crates/snitchwatch-bridge/src/rule_policy.rs` (operand/duration/name policy, fixed reasons) | whichever of **P2.7 / P2.1** lands first (expected P2.7) | prompt-slot D, #46 Part 2, Kirigami editor (via the `snitchwatch-bridge` dependency) |
| `crates/snitchwatch-bridge/src/daemon_config.rs` (`DefaultAction`, `Stats.MaxEvents`, `Rules.EnableChecksums` from `ClientConfig.config`) | whichever of **prompt-slot C / P2.6 Part 1** lands first | the editor and simulator (hash warnings) |
| `RulesSync::revision()` (#48 cache change counter) | P2.7 | P2.1 (stale edit checks, optional) |
| `bindable_process_path` (bridge) + `bindableProcessPath` role (Kirigami) | #44 Part A | inline Deny, prompt-slot C (P-c fallback), editor prefill |
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
| #48 `cache/rules.rs` (visibility, `revision`) | | | | ✓ | ✓ prune → hits | ✓ |
| new: `prompt_slot.rs`, `daemon_config.rs`, `rule_policy.rs`, `rule_io.rs`, `cache/rule_hits.rs` | | | ✓ / ✓ | ✓ / ✓ | ✓ | ✓ |
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

**Inline Deny**
- **D1:** "Deny all (N)" also until-restart app-bound?
  **Recommendation:** yes.
- **D2:** a hint under the sheet's "This time" Deny?
  **Recommendation:** add the hint.
- **D3:** rename the sheet's "Until quit" to "Until firewall restarts"?
  **Recommendation:** yes.

**Prompt slot**
- **S1:** silent auto-answer: on/off, the timeout, and the reply.
  **Recommendation:** on, 30 s, daemon default (P-a).
- **S2:** "Decide later" semantics.
  **Recommendation:** block the program for 5 min (P-c).
- **S3:** curated defaults: opt-in or on by default, the list,
  `kioworker`/Steam scope, per-user path regexps.
  **Recommendation:** opt-in; `/usr` only; host-constrained; no regexps
  in v1.
- **S4:** which daemon-side options to ask tower for.
  **Recommendation:** E3, then E2 (needs your OK), then E1 upstream.
- **S5:** answer from desktop notifications?
  **Recommendation:** Allow-once and Deny only.

**Import / export**
- **X1:** export contents.
  **Recommendation:** `always` + `until restart` user rules; no blocklist
  rules or temporaries.
- **X2:** same-name rows.
  **Recommendation:** replace, ticked, except loosening ones.
- **X3:** "replace all".
  **Recommendation:** not in v1.
- **X4:** accept opensnitchd on-disk rule files.
  **Recommendation:** v1.1.

**Insights**
- **N1:** persist hit counts before P3.1.
  **Recommendation:** yes.
- **N2:** the "unused" window.
  **Recommendation:** 14 days, only with N1.

**Editor**
- **E1:** rename.
  **Recommendation:** allow, as a two-step.
- **E2:** `process.path` patterns.
  **Recommendation:** exact must be absolute; a pattern is allowed with a
  warning.
- **E3:** `precedence`/`nolog` in v1.
  **Recommendation:** yes, under "Advanced".

## Verification constraint (all plans)

Another session runs timing-sensitive VM tests on this host:
- run cargo with `nice -n 19`, one crate at a time, with
  `CARGO_TARGET_DIR` at the `/var/home/...` spelling;
- never point a test at the live `opensnitchd`;
- any test that starts a bridge isolates `XDG_RUNTIME_DIR` (and, after
  #45 PR A, the state directory) through `Command::env`, never
  `std::env::set_var`.
