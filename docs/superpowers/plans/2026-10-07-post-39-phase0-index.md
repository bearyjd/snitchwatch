# Post-#39 Phase 0 work: index and order

**Date:** 2026-10-07
**Scope:** the roadmap Phase 0 items (`docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`)
that wait for draft PR #39 to merge. They wait because they touch files
#39 rewrites (`snitchwatch-bridge-cli/src/lib.rs`, `grpc_server.rs`,
`cache/connections.rs`, `ws_server.rs`) or change a contract those files
rely on.

**Baseline for every plan:**
- #39 at `5c2b44a`;
- the pending pause-before-GUI fix (branch `fix/39-pause-before-gui-check`),
  which moves the "no authenticated GUI → `Unavailable`" check ahead of the
  paused auto-allow branch in `ask_rule`;
- `main` @ `f65a2a4`, including #50.

## Prerequisites

1. **#39 merged, with the pause-before-GUI fix.** It closes the HIGH half
   of #47. It moves bazzite-tower's pinned head, so the #39 owner decides
   when.
2. **The honest-ui PR merged** (branch `fix/honest-ui`):
   - "Preview — not enforced" banners on `BlocklistsPage.qml` and
     `ProfilesPage.qml`;
   - plain-text labels on `RulesPage.qml`;
   - countdown and zero-byte UI hidden (the first two bullets of #49).

   Several plans below edit those same QML files and its guard tests
   (`crates/snitchwatch-kirigami/tests/honest_ui_qml_guards.rs`,
   `honest_ui_pages_qml.rs`).

## Plans

| Issue | Plan | Size | Depends on |
|---|---|---|---|
| #49 (3rd bullet) | `2026-10-07-stuck-pending-rows-after-39.md`. Bridge side **already fixed by #39** (`PendingCleanup`, test `tonic_request_deadline_cleans_pending_with_silent_authenticated_gui`). Remainder: an end-to-end test, a GUI inspector fix, a VM check | S | #39, honest-ui |
| #48 | `2026-10-07-show-all-daemon-rules.md`. `RulesCache` from `Subscribe`, `DaemonCommands` reply correlation, rules in `RequestSnapshot` | M | #39, honest-ui |
| #47 | `2026-10-07-timed-filter-pause.md`. `FilterPause` with 5/30/60 min only, a pause-aware tray choke point, `FilterPauseState` message | S–M | #39 + pause fix |
| #44 (2nd half) | `2026-10-07-app-bound-prompt-scopes-part2.md`. **A:** refuse remembered rules without `process_path` (once-only reply). **B:** flag pre-#50 host-only rules | S + S | A: #39 (best after #47 and #48). B: #48 |
| #45 | `2026-10-07-blocklist-enforcement.md`. **PR A:** wire, persist, refresh, honest status. **PR B:** `lists.domains` list directory, sink, reconcile, banner removal | S–M, then M | A: #39. B: #48 |
| #46 | `2026-10-07-profile-enforcement.md`. **Part 1:** persistence and banner wording. **Part 2:** enforcement. Lowest priority: no GUI can create profile rules yet | S, then M | Part 1: #45 PR A. Part 2: #45 PR B and the P2.1 editor |

## Recommended order

1. **#49 remainder.** Smallest and independent. It can start the day #39
   merges.
2. **#47 and #48 in parallel; merge #47 first.**
   - #47 is the remaining security-relevant item (MEDIUM: no expiry, tray
     loses the pause).
   - #48 is the foundation: #45 PR B, #44 B and #46 Part 2 all need its
     `RulesCache` and `DaemonCommands`.
   - Expect a trivial rebase of #48 onto #47 in the snapshot handler and
     `ws_messages.rs`.
3. **#45 PR A.** It can run in parallel with step 2: it only adds pump
   routing, stores and a refresh loop. It makes subscriptions real and
   honest ("not enforced: no rule sink") even before enforcement lands.
4. **#44 A and #44 B.**
   - **A** after #47 and #48: all three edit `ask_rule`.
   - **B** right after #48: it is GUI-only.
5. **#45 PR B**, after #48. This is the actual blocking.
6. **#46 Part 1** (any time after #45 PR A). **#46 Part 2** together with
   the P2.1 rule editor.

## File-conflict map

Lines are #39's. Edits to the same function are the conflicts that matter.

| File (function) | #49 | #48 | #47 | #44A | #44B | #45A | #45B | #46 |
|---|---|---|---|---|---|---|---|---|
| `bridge-cli/src/lib.rs` `run_with_incoming` (stores, spawns) | | ✓ | ✓ | | | ✓ | ✓ | ✓ |
| `bridge-cli/src/lib.rs` inbound pump (`:515-642`) | | ✓ rule effects | ✓ `SetFilteringPaused` | | | ✓ blocklist routing | | |
| `bridge-cli/src/lib.rs` `RequestSnapshot` (`:553-584`) | | ✓ | ✓ | | | | | |
| `grpc_server.rs` `ask_rule` | | ✓ cache upsert | ✓ paused branch | ✓ | | | | |
| `grpc_server.rs` `subscribe` / `notifications` | | ✓ (adds the signals #45 B consumes) | | | | | | |
| `cache/connections.rs` | | | ✓ `republish_pending_count` | | | | | |
| `ws_messages.rs` | | ✓ docs | ✓ new variants | ✓ new variant | | | ✓ summary fields | ✓ |
| `ws_server.rs` | | | ✓ peer uid (optional step) | | | ✓ event pump factored out | | |
| `blocklists/*` | | | | | | ✓ | ✓ | |
| `profiles/*` | | | | | | | | ✓ |
| `tests/bridge_protocol_test.rs` | ✓ | ✓ | | | | | ✓ | |
| QML: `ConnectionsPage` / `PendingDecisionSheet` | ✓ | | | ✓ | | | | |
| QML: `RulesPage` | | | | | ✓ | | ✓ comment | |
| QML: `BlocklistsPage` + guards | | | | | | ✓ wording | ✓ | |
| QML: `ProfilesPage` + guards | | | | | | | | ✓ |
| QML: `main.qml` tray, Kirigami `tray.rs` | | | ✓ | | | | | |

Every row above also conflicts with the honest-ui PR wherever a QML file
is listed. That is why it is a prerequisite.

## Cross-cutting findings recorded in the plans

- **Production never routes blocklist messages to `BlocklistsManager`.**
  `SubscribeBlocklist` becomes `UpstreamEffect::None` (#39
  `translator/upstream.rs:83-96`), and only the test helper
  `serve_with_blocklists` has the event pump. #45 PR A fixes this.
- **Outbound rule commands are fire-and-forget.** The daemon's
  `NotificationReply` is only logged, and nothing is broadcast back after a
  toggle or delete. #48's `DaemonCommands` fixes this, and every later
  "enforced" status depends on it.
- **"User rules always win" is false** under opensnitchd's evaluation
  (`vendor:daemon/rule/loader.go:497-515`) and the owner's
  blocklist-wins decision. #45 corrects the docs; #46 surfaces the
  profile-allow precedence question.
- **The bridge's own blocklist downloads go through opensnitchd.** In
  system mode with no GUI attached they hit the default deny. This policy
  question is shared with Phase 1's deny-by-default decision.

## Verification constraint (all plans)

Another session runs timing-sensitive VM tests on this host. Run cargo
commands with `nice -n 19`, one crate at a time. Never point a test at the
live `opensnitchd`. Any test that starts a bridge must isolate
`XDG_RUNTIME_DIR`, and with #45, the state directory as well (CLAUDE.md).
