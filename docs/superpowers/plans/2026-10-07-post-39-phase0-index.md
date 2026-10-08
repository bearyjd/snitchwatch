# Post-#39 Phase 0 work: index and order

**Date:** 2026-10-07 (revised after #39 merged)
**Scope:** the roadmap Phase 0 items
(`docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`) that
waited for #39, because they touch files #39 rewrote or a contract those
files rely on.

**Baseline for every plan:** `main` @ `670f42c`, the merge of #39. It
includes these commits:

| Commit | Change |
|---|---|
| `467201f` | `ask_rule` admits an authenticated GUI before the pause shortcut |
| `a3cdbb1` | `clear_pause_on_last_session_loss` clears a pause when the last GUI session ends |
| `579a87c` | `apply_pause_request` ignores a pause request with no GUI attached |
| `a2a5f3b` | The pause is set inside `Admission::while_current`. `RunningBridge.client_presence` is `#[cfg(test)]` |
| `f65a2a4` | #50, the first half of #44 |

**Go by function names, not line numbers.** Line numbers in these plans
are approximate pointers as of `670f42c` and will drift. The named
function or test is authoritative.

## Prerequisite

The honest-ui PR (branch `fix/honest-ui`, not yet on `origin`) must merge
first:
- "not applied" banners on `BlocklistsPage.qml`/`ProfilesPage.qml`;
- plain-text labels everywhere;
- the countdown and zero-byte UI hidden, which covers the first two bullets
  of #49.

Several plans edit those QML files and its guard tests
(`crates/snitchwatch-kirigami/tests/honest_ui_qml_guards.rs`
`assert_preview_banner`, `honest_ui_pages_qml.rs`).

## Plans

| Issue | Plan | Size | Depends on |
|---|---|---|---|
| #49 (3rd bullet) | `2026-10-07-stuck-pending-rows-after-39.md`. Bridge side **fixed by #39** (`PendingCleanup`; test `tonic_request_deadline_cleans_pending_with_silent_authenticated_gui`). Left: an end-to-end test, a GUI re-check of the open inspector on remove, clear and reconnect, and a VM check | S | honest-ui |
| #48 | `2026-10-07-show-all-daemon-rules.md`. `RulesCache` from `Subscribe`, committed when that connection sends HELLO and preserving `created` on toggles; `DaemonCommands` reply correlation tied to the current HELLO stream; the mock sends HELLO; rules in the snapshot | M | honest-ui |
| #47 | `2026-10-07-timed-filter-pause.md`. `FilterPause` with 5/30/60 min, ported into `apply_pause_request`/`clear_pause_on_last_session_loss`; sender-generation stamp closes the queued-pause race; pause-aware tray choke point | S–M | — |
| #44 (2nd half) | `2026-10-07-app-bound-prompt-scopes-part2.md`. **A:** no remembered rule unless `process_path` is absolute (once-only reply plus explanation). **B:** flag pre-#50 host-only rules, with one-click delete per row and an explicit unblock warning on deny rows | S + S | A: none (rebase on #47/#48). B: #48 |
| #45 | `2026-10-07-blocklist-enforcement.md`. **PR A:** https-only bounded fetcher, a test fetch hook, a single worker, stable ids, persistence, honest status. **PR B:** `lists.domains` list directory, sink, reconcile, banner removal | S–M, then M | A: honest-ui. B: #48 |
| #46 | `2026-10-07-profile-enforcement.md`. **Part 1:** persistence and banner wording. **Part 2:** enforcement, with `process.path` forced `sensitive: true`. Lowest priority | S, then M | Part 1: #45 PR A. Part 2: #45 PR B and the P2.1 editor |

## Recommended order

1. **#49 remainder.** Small and independent.
2. **#47 and #48 in parallel; merge #47 first** (security-relevant and
   smaller). Then rebase #48; expect trivial conflicts in the pump,
   snapshot handler and `ws_messages.rs`.
3. **#45 PR A.** It can run alongside step 2. **It closes a security hole
   before persistence lands:** today the fetcher accepts `file://` with no
   cap, so a `snitchwatch-ui` member could OOM the system bridge.
4. **#44 A** after #47 and #48, since all three edit `ask_rule`.
   **#44 B** right after #48.
5. **#45 PR B**, after #48.
6. **#46 Part 1** any time after #45 PR A. **#46 Part 2** with the P2.1
   editor.

## File-conflict map (by function)

| File (function) | #49 | #48 | #47 | #44A | #44B | #45A | #45B | #46 |
|---|---|---|---|---|---|---|---|---|
| bridge-cli `lib.rs` `run_with_incoming` (stores, spawns, `RunningBridge`) | | ✓ | ✓ | | | ✓ | ✓ | ✓ |
| bridge-cli `lib.rs` inbound pump | | ✓ rule-effect arm | ✓ `SetFilteringPaused` arm | | | ✓ blocklist routing to worker | | |
| bridge-cli `lib.rs` `SnapshotRequested` arm | | ✓ | ✓ | | | | | |
| bridge-cli `main.rs` (state-dir resolver, `run_with_options`) | | | | | | ✓ | | ✓ |
| `client_presence.rs` (`apply_pause_request`, `clear_pause_on_last_session_loss`, new `while_generation_current`/`current_generation`) | | | ✓ | | | | | |
| `ws_server.rs` (`serve`, `pump_authenticated`, `serve_with_blocklists`) | | | ✓ peer uid, generation stamp | | | ✓ event pump factored out | | |
| `grpc_server.rs` `ask_rule` | | ✓ cache upsert | ✓ `FilterPause` | ✓ | | | | |
| `grpc_server.rs` `subscribe` / `notifications` | | ✓ (adds the signals #45B uses) | | | | | | |
| `cache/connections.rs` `republish_pending_count` | | | ✓ | | | | | |
| `translator/verdict.rs` | | | | ✓ | | | | |
| `notice.rs`, tauri `notifier.rs`, kirigami `notifier.rs` / `notification_controller.rs` | | | | ✓ new `Notice` variant | | | | |
| `ws_messages.rs` | | ✓ docs | ✓ variants and `sender_generation` | ✓ variant | | ✓ `BlocklistSummary` fields | | ✓ `SetProfiles` fields |
| `translator/downstream.rs` | | | | | | ✓ `build_set_blocklists`/`_status` | | ✓ `build_set_profiles` |
| `blocklists/*` (`fetcher.rs`, `mod.rs`, `materializer.rs`) | | | | | | ✓ | ✓ | |
| `profiles/*` | | | | | | | | ✓ |
| `tests/bridge_protocol_test.rs`, `tests/mock_opensnitchd` (#48: `open_notifications` sends HELLO and tests await `daemon_stream_ready`; #47: the mock's `spawn_bridge_grpc` test helper calls `UiService::new`) | ✓ | ✓ | ✓ | ✓ | | | ✓ | |
| QML: `ConnectionsPage` / `PendingDecisionSheet` | ✓ | | | ✓ | | | | |
| QML: `RulesPage` | | | | | ✓ | | ✓ comment | |
| QML: `BlocklistsPage` and guards | | | | | | ✓ wording | ✓ | |
| QML: `ProfilesPage` and guards | | | | | | | | ✓ |
| QML: `main.qml` tray, Kirigami `tray.rs`/`tray_controller.rs`/`bridge_runtime.rs` | | | ✓ | | | | | |

Every QML row also conflicts with the honest-ui PR.

## Cross-cutting findings recorded in the plans

- **Blocklist messages never reach the manager.** Production routes
  `SubscribeBlocklist` to `UpstreamEffect::None`, and only a test helper
  has the event pump (#45 PR A).
- **The blocklist fetcher is unsafe for user-supplied URLs:** `file://`
  with no cap, `http`, and an unbounded chunked body. #45 PR A fixes this
  before persistence makes an OOM repeat on every restart.
- **Outbound rule commands are fire-and-forget**, and they fan out to every
  open daemon stream (#48).
- **"User rules always win" is false** under opensnitchd's `FindFirstMatch`
  and the blocklist-wins decision. #45 fixes the docs; #46 raises the
  profile precedence question.
- **The bridge's own blocklist downloads go through opensnitchd** and hit
  the default deny when no GUI is attached. This is part of the Phase 1
  policy decision.

## Verification constraint (all plans)

Another session runs timing-sensitive VM tests on this host. Run cargo
with `nice -n 19`, one crate at a time. Never point a test at the live
`opensnitchd`. Any test that starts a bridge isolates `XDG_RUNTIME_DIR`,
and with #45 also the state directory. Pass both through `Command::env` for
subprocesses; never use `std::env::set_var` in-process.
