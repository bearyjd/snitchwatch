# Timed filter pause that the tray cannot lose (issue #47)

**Date:** 2026-10-07
**Issue:** #47 (roadmap P0.5)
**Blocked on:** draft PR #39 merging. #39 rewrites `ask_rule` and the
bridge-cli pump. This plan assumes #39 includes the separate small fix that
moves the "no authenticated GUI → `Unavailable`" check *before* the paused
auto-allow branch (branch `fix/39-pause-before-gui-check`). With that fix,
a paused bridge no longer auto-allows when no GUI is attached, which closes
the HIGH part of #47.
**Severity after that fix:** MEDIUM. A pause never ends, and the tray can
show "filtering" while the pause is still on.
**Size:** S–M.

## Owner decision (settled)

**Timed only.**
- The tray offers 5 min, 30 min and 1 hour.
- A pause always expires on its own and emits the existing
  `Notice::FilterPauseExpired`.
- The pause is broadcast as its own state.
- Resyncing the tray honors the pause.

## Citation convention

- `#39:` is PR #39 at `5c2b44a`.
- `main:` is `f65a2a4`. Files #39 does not touch have the same lines on
  both: `daemon_watchdog.rs`, `tray_state.rs`, `notice.rs`, `ws_messages.rs`,
  Kirigami `tray.rs`, `tray_controller.rs`, `main.qml` and
  `notification_controller.rs`.

The pause-before-GUI fix shifts the lines inside `ask_rule` by about +5,
so the function name is always given.

## Goal

1. A pause is always one of 5 min, 30 min or 1 hour, and it ends on its
   own.
2. When it ends, every GUI gets the expiry notice and the new pause state,
   and prompting resumes.
3. While paused, no code path can put the tray back to Idle or Pending.
4. A GUI that connects mid-pause learns the pause state and the remaining
   time.

## Out of scope

- Untimed or indefinite pause. The owner has rejected it.
- Silent-allow, learning or lockdown modes (P2.3).
- Persisting a pause across bridge restarts. A restart unpauses, which
  fails closed; keep that.
- Polkit or role gating of who may pause. In system mode, any
  `snitchwatch-ui` member can still pause (see
  `docs/packaging/system-bridge-integration.md` on #39). This plan only
  logs who did it (step 6).

## Findings (reset paths, #39 equivalents)

All tray resets eventually call `ConnectionCache::republish_pending_count`
(`#39:crates/snitchwatch-bridge/src/cache/connections.rs:88-97`). That
function publishes `Idle` or `Pending(n)` and ignores the pause. Its
callers:

1. **Daemon watchdog recovery:**
   `main:crates/snitchwatch-bridge/src/daemon_watchdog.rs:58-62` calls
   `resync_tray_state()`. #39 leaves this file unchanged.
2. **Recent-block revert timer:** `#39:grpc_server.rs:364-382`
   (`publish_recent_block`). `resync_tray_state()` is at `:379`.
3. **Resolving a pending row:** `#39:connections.rs:162-217` (`resolve`).
   It republishes at `:215`, and on the undelivered path at `:192`.
   (This was `main` `:81-99`.)
4. **New in #39:**
   - `cancel_pending`, `:220-228` (republish at `:226`), which handles RPC
     cancellation and GUI loss;
   - `insert_pending_inner`, `:156`, for a pending row inserted before the
     pause.

Other facts:

- **The flag.** Pause is a bare `Arc<AtomicBool>`, created at
  `#39:crates/snitchwatch-bridge-cli/src/lib.rs:378-381`.
- **Setting it.** It is set in the inbound pump at `#39:lib.rs:520-528`.
  Pausing sets `TrayState::FilterOff` directly. Resuming calls
  `resync_tray_state()`.
- **Reading it.** `ask_rule` reads it in the paused branch at
  `#39:grpc_server.rs:470-497`, which auto-allows `Once`/`ThisHost`.
- **The message.** `ClientMessage::SetFilteringPaused { paused }`
  (`main:crates/snitchwatch-bridge/src/ws_messages.rs:281-289`) carries no
  duration. Every constructor:
  - `#39:lib.rs:520`, `1327`, `1335`
  - `translator/upstream.rs:95`
  - `ws_messages.rs:287`, `931`
  - Kirigami `tray.rs:64`, `132`
- **The expiry notice.** `Notice::FilterPauseExpired` (`notice.rs:19`) is
  only built in a test (`notice.rs:93`). The Kirigami
  `notification_controller.rs:211` already renders it.
- **The tray menu.** The tray offers a single toggle:
  - `main:crates/snitchwatch-kirigami/qml/main.qml:476-487`;
  - `derive_menu_label` at `tray.rs:35-43` maps `FilterOff` to "Resume" and
    everything else to "Pause".
  That is why a reset to Idle re-offers "Pause filtering".
- **Protocol constraint.** `TrayState` (`tray_state.rs`) is externally
  tagged, and `FilterOff` is a unit variant in the published 0.1.1
  protocol. Don't change its shape; add a separate message instead.

## Design

1. **`FilterPause`** (new file `crates/snitchwatch-bridge/src/filter_pause.rs`).
   It replaces the `AtomicBool`.
   - **State:** `StdMutex<Option<Active>>`, where
     `Active { mono_deadline: tokio::time::Instant, wall_deadline: SystemTime, generation: u64 }`.
   - **Operations:**
     - `pause(duration) -> Result<PauseState, Rejected>`
     - `resume() -> PauseState`
     - `is_active(now_mono, now_wall) -> bool`
     - `state() -> PauseState { paused, expires_at_unix_ms }`
   - **Clocks:** a pause is active only while *both* deadlines are in the
     future. The monotonic clock does not advance during suspend on Linux,
     so the wall clock ends a 1-hour pause that spans an overnight suspend.
     The monotonic clock covers a wall clock stepped backwards.
   - **Allowed durations:** 300, 1800 and 3600 s (constant
     `ALLOWED_PAUSE_SECS`). Anything else is `Rejected`.
   - **Who uses it:** `UiService::new` takes `Arc<FilterPause>` in place of
     `Arc<AtomicBool>`, a mechanical change at 19 call sites, most of them in
     `grpc_server/tests.rs`. The paused branch of `ask_rule` checks
     `is_active(now)`, so an expired pause stops auto-allowing even if the
     timer task (step 3) has not run yet.
2. **One choke point for the tray.**
   - `ConnectionCache` gets `with_filter_pause(Arc<FilterPause>)`.
     `republish_pending_count` publishes `FilterOff` while the pause is
     active, and otherwise `Idle`/`Pending(n)` as now. This covers every path
     listed in Findings.
   - `DaemonDown` (`daemon_watchdog.rs:49`) and the transient `RecentBlock`
     still override, because they are more urgent. Their own revert paths
     go through the cache.
   - **Rejected alternative:** an overlay in `TrayStatePublisher`. That
     would also change what the retiring Tauri shell sees, for no benefit.
3. **Expiry task.** It starts in `run_with_incoming` next to the watchdog
   and runs a 1 s interval tick. A tick is more robust across suspend than
   one long `sleep_until`. When it sees a pause go from active to expired
   (checked with a generation match, so a resume or re-pause in between is
   a no-op), it:
   - clears the pause;
   - sends `Notice::FilterPauseExpired` on the `NoticeBus`, which already
     reaches external GUIs as `ServerMessage::Notice`
     (`#39:lib.rs:360-376`);
   - broadcasts the new pause state;
   - calls `cache.resync_tray_state()`.
4. **Protocol.** All changes are additive.
   - **Client message:** `SetFilteringPaused { paused, #[serde(default)] duration_secs: Option<u64> }`.
   - **Pump** (`#39:lib.rs:520-528`):
     - `paused: true` with an allowed duration calls `pause`;
     - `paused: true` with `None` comes from an older client and gets 300 s
       (see Risks);
     - a disallowed value is logged and changes nothing;
     - `paused: false` calls `resume`.
     After any of these, the pump broadcasts the pause state and resyncs
     the tray.
   - **New server message:**
     `ServerMessage::FilterPauseState { paused: bool, expires_at_unix_ms: Option<u64> }`.
     Older clients ignore unknown actions (`#39:lib.rs:344-346`).
   - **Snapshot:** add the message to the `RequestSnapshot` answer
     (`#39:lib.rs:553-584`).
   - **Docs:** update the comment at `ws_messages.rs:281-286`.
5. **Kirigami.**
   - **Tray state** (`tray.rs`): add `PauseView { until: Option<…> }`.
     `derive_menu_label` gains a pause-aware variant:
     - not paused: a "Pause filtering" submenu with 5 min / 30 min / 1 hour;
     - paused: "Resume filtering (until HH:MM)".
     The tooltip shows the end time.
     `build_set_filtering_paused_json(paused, duration_secs)`.
   - **Controller:** `TrayController` gains a `pausedUntil` qproperty,
     `pauseFor(secs)` and `resume()`.
   - **Routing:** route `FilterPauseState` the way #39's
     `bridge_runtime.rs` routes `ServerMessage::TrayState`
     (`#39:crates/snitchwatch-kirigami/src/bridge_runtime.rs:428`, into
     `ReceivedTrayState` at `:45`). Like tray state, it must go through the
     session-labelled stale-frame guard.
   - **QML:** `main.qml:476-487` gets a submenu.
6. **Log who paused** (separable; can be a follow-up PR).
   - `#39:crates/snitchwatch-bridge/src/ws_server.rs:154` discards the peer.
     Capture `stream.peer_cred()` uid at accept and pass it into
     `pump_authenticated` (`:236-279`).
   - When a parsed message is `SetFilteringPaused`, log
     `info!(uid, paused, duration_secs, …)` there.
   - In-process senders through `RunningBridge::inbound_tx` log as
     "in-process".
7. **Docs.**
   - If the #39 fix has not already done so, correct the line "Paused
     auto-allow remains unchanged." in `docs/packaging/system-bridge-integration.md`
     (#39 `:40`).
   - Note the new behavior in `HANDOFF.md`.

## Tests to write first

Bridge unit tests. Use `tokio::test(start_paused = true)` where time
matters, and inject a wall clock into `FilterPause`.

- **Durations:** `pause(300 s)` is active, then inactive after advancing
  300 s. 0, 301 and 7200 are rejected and leave the state unchanged.
- **Clock jumps:** a wall-clock jump past the deadline ends the pause
  while the monotonic clock has not elapsed. A backwards wall jump does
  not extend it.
- **Expiry task:**
  - exactly one `FilterPauseExpired` notice;
  - exactly one `FilterPauseState{paused:false}`;
  - the tray goes back to `Idle`;
  - a re-pause before expiry suppresses the old generation's expiry.
- **Each reset path while paused** must publish `FilterOff`, not
  `Idle`/`Pending`:
  - watchdog recovery (extend the watchdog test module;
    `daemon_watchdog.rs` tests already use a paused clock);
  - the `RecentBlock` revert after a pre-pause pending row resolves as Deny
    (pattern: #39 `grpc_server/tests.rs:623`
    `ask_rule_deny_publishes_recent_block_then_reverts_to_idle`);
  - `resolve` of a pre-pause pending row;
  - `cancel_pending`.
- **`ask_rule`:**
  - with an expired pause whose timer has not fired yet, it prompts and
    does not auto-allow;
  - the existing `ask_rule_auto_allows_immediately_when_filtering_paused`
    (#39 `tests.rs:778`) keeps passing with a GUI lease.
- **Serde:**
  - legacy `{"action":"setFilteringPaused","paused":true}` parses with
    `duration_secs: None`;
  - the new field round-trips.
- **bridge-cli:**
  - extend `set_filtering_paused_toggles_tray_state` (#39 `lib.rs:1316`)
    with a duration;
  - `RequestSnapshot` includes `FilterPauseState`;
  - a legacy message without a duration expires after 300 s.

Kirigami unit tests (`tray.rs` is pure):

- the menu model for not-paused, paused and daemon-down;
- the tooltip with an end time;
- the JSON carries `durationSecs`.

## Verification

Run at low priority:

- `cargo test -p snitchwatch-bridge`
- `cargo test -p snitchwatch-bridge-cli`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -p snitchwatch-kirigami tray`, with
  `QT_QPA_PLATFORM=offscreen`

Manual VM checks:

1. Pause for 5 min and generate traffic: rows auto-allow. A daemon
   restart during the pause keeps the tray on FilterOff after recovery.
2. At expiry, a desktop notification appears and the next connection
   prompts.
3. Suspend the VM across a pause deadline: on resume, the pause is over
   within 1 s.

## Risks and open questions

- **Legacy `{paused:true}` without a duration.** The plan defaults it to
  300 s. The alternative is to reject it, which would break an unupgraded
  tray's toggle; that is safe but confusing. Owner's call. The plan
  assumes 300 s.
- **Tray priority while paused.** Should `Pending(n)` from pre-pause rows
  still show while paused? This plan shows `FilterOff`, because the pause
  is the security-relevant state. Pending rows stay in the Connections list
  and keep their prompts.
- **What should end a pause.** Should the last GUI disconnecting also end
  it? With the #39 fix, a paused bridge without a GUI already returns
  `Unavailable`, so this is no longer a bypass. Leave it timed-only unless
  the owner wants both.
- **File-conflict hot spots:**
  - with #48: `#39:lib.rs` pump and snapshot handler, `ws_messages.rs`;
  - with #44 part 2: `grpc_server.rs` `ask_rule`.
  They can be developed in parallel, but merge #47 first (smaller and
  security-relevant), then rebase.
