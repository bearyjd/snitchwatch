# Timed filter pause that the tray cannot lose (issue #47)

**Date:** 2026-10-07 (revised after #39 merged as `670f42c`)
**Issue:** #47 (roadmap P0.5)
**Baseline:** `main` @ `670f42c`.

That merge includes #39 and four pause fixes. They closed the HIGH part of
#47, and this plan **keeps their semantics**:

| Commit | Fix |
|---|---|
| `467201f` | `ask_rule` admits the GUI before the pause shortcut, so a paused bridge with no GUI returns `Unavailable` |
| `a3cdbb1` | `clear_pause_on_last_session_loss` clears the pause when the last GUI session ends |
| `579a87c` | `apply_pause_request` ignores a pause request that arrives with no GUI attached |
| `a2a5f3b` | The pause flag is set inside `Admission::while_current`. `RunningBridge.client_presence` is `#[cfg(test)]` |

**Remaining severity:** MEDIUM. A pause has no expiry while a GUI stays
attached, and three paths reset the tray to Idle mid-pause.
**Size:** S–M.

## Owner decision (settled) and how this plan reads it

**Timed only.**
- The tray offers 5 min, 30 min and 1 hour.
- A pause always ends on its own and emits the existing
  `Notice::FilterPauseExpired`.
- The pause is broadcast as its own state.
- Resyncing the tray honors the pause.

"Timed only" means **no indefinite pause**. It does **not** mean the timer
is the only thing that ends a pause:
- the last GUI session ending still clears it (`a3cdbb1`);
- a pause request with no GUI attached is still ignored (`579a87c`).

## Citation convention

`main:` means `670f42c`. Functions and tests are cited by name. Line
numbers are approximate pointers only, so go by the name.

## Goal

1. A pause is exactly 5 min, 30 min or 1 hour.
2. It ends at the earliest of: expiry, an explicit resume, or the last
   authenticated GUI session ending.
3. Expiry emits `Notice::FilterPauseExpired` plus the new pause state, and
   prompting resumes.
4. While paused, no code path puts the tray back to Idle or Pending.
5. A GUI that connects mid-pause learns the state and the end time.
6. A pause request applies only if its *sender's* GUI session generation
   is still current. This closes the race `apply_pause_request` documents.

## Out of scope

- An untimed pause.
- Persisting a pause across bridge restarts. A restart unpauses, which
  fails closed.
- Learning and lockdown modes (P2.3).
- Polkit or role gating of who may pause. In system mode, any
  `snitchwatch-ui` member still may. This plan only logs the uid.

## Findings (`main`)

**Pause plumbing today:**
- **The flag.** `Arc<AtomicBool>` `filtering_paused`, created in
  `run_with_incoming` (`snitchwatch-bridge-cli/src/lib.rs`, ~387).
- **Cleared on last session loss.** The task
  `client_presence::clear_pause_on_last_session_loss` does this, and its
  `on_cleared` callback calls `cache.resync_tray_state()`.
- **Pause requests.** The inbound pump's `SetFilteringPaused` arm (~544)
  calls `client_presence::apply_pause_request`:
  - "resume" stores `false`;
  - "pause" stores `true` only inside
    `presence.admit()…while_current(…)`.
  The pump then sets `TrayState::FilterOff` or resyncs.
- **The documented race.** `apply_pause_request`'s doc comment (and
  `docs/packaging/system-bridge-integration.md`, "Unattended requests")
  names it: the admission is taken when the pump *applies* the request, not
  when the sender sent it. A pause queued by a GUI that has since left
  applies anyway if another GUI authenticated in between.
- **Reading the flag.** `grpc_server.rs` `ask_rule` admits first (`467201f`),
  then reads `filtering_paused` and auto-allows `Once`/`ThisHost`.

**Tray reset paths.** All of them end in
`ConnectionCache::republish_pending_count` (`cache/connections.rs`), which
publishes `Idle`/`Pending(n)` and ignores the pause. Its callers:
1. `daemon_watchdog::run` on recovery (`resync_tray_state`);
2. `UiService::publish_recent_block`'s revert timer (`resync_tray_state`);
3. `ConnectionCache::resolve`, on success and on the undelivered path;
4. `ConnectionCache::cancel_pending`, for RPC cancellation and GUI loss;
5. `insert_pending_inner`;
6. the pause-clear callback above.

**Message and tray constructors:**
- `ClientMessage::SetFilteringPaused { paused }` (`ws_messages.rs`) has no
  duration. Constructors: bridge-cli pump and tests, `translator/upstream.rs`
  `apply`, `ws_messages.rs` tests, Kirigami
  `tray::build_set_filtering_paused_json`. Grep
  `SetFilteringPaused {` before editing.
- `Notice::FilterPauseExpired` is only built in a `notice.rs` test. The
  Kirigami `notification_controller.rs` already renders it.
- `TrayState::FilterOff` is a unit variant of an externally tagged enum in
  the published 0.1.1 protocol. Don't change its shape.
- The tray menu has one toggle: `main.qml` "Pause/Resume filtering",
  driven by `tray::derive_menu_label`.

**Session identity:**
- `ws_server.rs` `WsServer::serve` discards the peer address on accept.
- `pump_authenticated` holds the `SessionLease` and forwards each parsed
  `ClientMessage` with no origin.

## Design

1. **`FilterPause`** (new file `crates/snitchwatch-bridge/src/filter_pause.rs`).
   It replaces the `AtomicBool`.
   - **State:** `StdMutex<Option<Active>>`, where
     `Active { mono_deadline: tokio::time::Instant, wall_deadline: SystemTime, generation: u64 }`.
   - **Operations:**
     - `pause(duration) -> Result<PauseState, Rejected>`
     - `resume() -> bool`, which reports whether a pause was cleared
     - `is_active(now_mono, now_wall)`
     - `state() -> PauseState { paused, expires_at_unix_ms }`
   - **Clocks:** a pause is active only while *both* deadlines are ahead.
     The monotonic clock doesn't advance during suspend; the wall clock
     covers that. The monotonic clock covers a wall clock stepped
     backwards. Inject the wall clock for tests.
   - **Allowed durations:** 300, 1800 and 3600 s (`ALLOWED_PAUSE_SECS`).
     Anything else is `Rejected`.
   - **Constructor change:** `UiService::new` takes `Arc<FilterPause>` in
     place of `Arc<AtomicBool>`, a mechanical change at every call site
     (`grpc_server/tests.rs`, bridge-cli, `tests/mock_opensnitchd`).
   - **Lazy expiry:** `ask_rule` checks `is_active(now)`, so an expired
     pause stops auto-allowing even before the expiry task runs. The
     admit-before-pause order from `467201f` stays as it is.
2. **Port the `client_presence` guards to `FilterPause`.** Never call
   `pause()` from a handler directly.
   - **`apply_pause_request(presence, &FilterPause, request, sender_generation)`.**
     - "resume" calls `resume()`.
     - "pause(d)" calls `pause(d)` **only inside**
       `presence.while_generation_current(sender_generation, || …)`, a new
       `ClientPresence` method that does the same check as
       `Admission::while_current` against a given generation.
     - With `sender_generation: None`, it falls back to today's
       `presence.admit()?.while_current(…)`. That covers in-process senders
       through `RunningBridge::inbound_tx`, which have no WS session.
   - **`clear_pause_on_last_session_loss(losses, Arc<FilterPause>, on_cleared)`**
     calls `resume()` on each last-session loss. If that cleared a pause,
     `on_cleared` resyncs the tray and broadcasts the new state.
     - There is no `FilterPauseExpired` notice here: no GUI is left to
       show it.
   - **Invariant kept:** the pause is never active with zero sessions. The
     set happens under the presence lock, and a racing loss either prevents
     it or follows it and clears it.
3. **Close the queued-pause race** (`apply_pause_request`'s "remaining
   gap").
   - In `ws_server.rs` `pump_authenticated`, read the presence's current
     `loss_generation` right after taking the `SessionLease`, through a new
     `ClientPresence::current_generation()`. That value is stable while the
     lease is held.
   - Stamp it on every forwarded `SetFilteringPaused` through a new field,
     `#[serde(skip)] sender_generation: Option<u64>`. With `skip`, a client
     cannot supply the value; the wire format is unchanged and the field
     deserializes as `None`.
   - Effect: a pause from a session whose generation ended (every GUI left)
     is ignored, even if a new GUI has authenticated since.
   - Update `apply_pause_request`'s doc comment and the "One race remains"
     sentence in `system-bridge-integration.md`.
4. **One choke point for the tray.**
   - `ConnectionCache::with_filter_pause(Arc<FilterPause>)`.
     `republish_pending_count` publishes `FilterOff` while the pause is
     active, and otherwise `Idle`/`Pending(n)`. This covers every path
     listed in Findings, including the pause-clear callback.
   - `DaemonDown` and the transient `RecentBlock` still override. Their
     reverts go through the cache.
   - The pump stops setting `FilterOff` directly and calls
     `resync_tray_state()` after any pause change.
5. **Expiry task.** It starts in `run_with_incoming` next to the clear task
   and runs a 1 s interval tick, which survives suspend better than one long
   `sleep_until`. When it sees a pause go from active to expired, with the
   generation matching so that a resume or re-pause in between is a no-op,
   it:
   - clears the pause;
   - sends `Notice::FilterPauseExpired`, which reaches external GUIs as
     `ServerMessage::Notice` through the existing notice relay;
   - broadcasts the new state;
   - resyncs the tray.
6. **Protocol.** All changes are additive.
   - **Client message:** `SetFilteringPaused` gains
     `#[serde(default)] duration_secs: Option<u64>`. A pause without a
     duration comes from an older client and gets 300 s (see Risks).
   - **Server message:**
     `ServerMessage::FilterPauseState { paused, expires_at_unix_ms }`.
     Older clients ignore unknown actions (comment above the tray relay in
     `run_with_incoming`).
   - **Snapshot:** add the message to the `RequestSnapshot` answer.
7. **Log who paused** (same `pump_authenticated` change as step 3).
   - In `WsServer::serve`, capture `stream.peer_cred()` uid at accept and
     pass it to `pump_authenticated`.
   - Log `info!(uid, paused, duration_secs, …)` there for each
     `SetFilteringPaused`.
   - In-process senders log as "in-process".
8. **Kirigami.**
   - **Tray** (`tray.rs`):
     - not paused: a "Pause filtering" submenu with 5 min / 30 min / 1 hour;
     - paused: "Resume filtering (until HH:MM)", with the time in the
       tooltip;
     - `build_set_filtering_paused_json(paused, duration_secs)`.
   - **Controller:** `TrayController` gains a `pausedUntil` qproperty,
     `pauseFor(secs)` and `resume()`.
   - **Routing:** route `FilterPauseState` through `bridge_runtime.rs` the
     way `ServerMessage::TrayState` is routed, including the
     session-labelled stale-frame guard (`ReceivedTrayState`).
   - **QML:** submenu in `main.qml`.

## Tests to write first

**Bridge unit tests** (use `tokio::test(start_paused = true)` where time
matters):

- **`FilterPause`:**
  - the three allowed durations work;
  - 0, 301 and 7200 are rejected and change nothing;
  - a wall jump past the deadline ends the pause;
  - a backwards wall jump doesn't extend it.
- **`client_presence`:** port the existing tests to `FilterPause` and keep
  them green:
  - `a_pause_request_takes_effect_only_with_an_authenticated_gui`;
  - `last_session_loss_clears_a_filtering_pause`;
  - `last_session_loss_without_a_pause_does_not_report_a_clear`.
- **New, the race:**
  1. session A authenticates and its stamp is taken;
  2. A disconnects (the generation advances);
  3. session B authenticates;
  4. A's stamped pause is applied, and is ignored.

  An unstamped (`None`) request with B present still applies.
- **Serde:** a client-supplied `senderGeneration` in JSON is ignored
  (it deserializes as `None`). Legacy `{"paused":true}` parses with
  `duration_secs: None`.
- **Expiry task:**
  - exactly one `FilterPauseExpired`;
  - one `FilterPauseState{paused:false}`;
  - the tray goes back to `Idle`;
  - a re-pause before expiry suppresses the old generation.
- **Each reset path while paused** publishes `FilterOff`:
  - watchdog recovery (extend the `daemon_watchdog` tests);
  - the recent-block revert after a pre-pause pending row resolves as Deny
    (pattern: `ask_rule_deny_publishes_recent_block_then_reverts_to_idle`);
  - `resolve`;
  - `cancel_pending`.
- **`ask_rule`:**
  - an expired pause whose timer hasn't fired yet prompts and does not
    auto-allow;
  - `paused_bridge_without_an_authenticated_gui_defers_to_the_daemon` and
    `ask_rule_auto_allows_immediately_when_filtering_paused` keep passing.

**bridge-cli tests** (`lib.rs` test module). They **must** register
`bridge.client_presence.authenticated_session()` (`#[cfg(test)]` field)
before pausing, as `set_filtering_paused_toggles_tray_state` already does:
- extend that test with a duration;
- `RequestSnapshot` includes `FilterPauseState`;
- a legacy message without a duration expires after 300 s;
- dropping the last lease clears the pause and broadcasts
  `FilterPauseState{paused:false}` with no expiry notice.

**Kirigami unit tests** (`tray.rs`, pure functions):
- the menu model for not-paused, paused and daemon-down;
- the tooltip;
- the JSON carries `durationSecs`.

## Verification

Run at low priority:

- `cargo test -p snitchwatch-bridge`
- `cargo test -p snitchwatch-bridge-cli`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test -p snitchwatch-kirigami tray`, with
  `QT_QPA_PLATFORM=offscreen`

Manual VM checks:

1. Pause for 5 min. Rows auto-allow, and a daemon restart keeps the tray on
   FilterOff.
2. At expiry, the next connection prompts.
3. Close the only GUI mid-pause, then reopen it: not paused.
4. Suspend across the deadline: the pause is over within 1 s of resume.

## Risks and open questions

- **Legacy `{paused:true}` with no duration** defaults to 300 s, so an
  unupgraded tray still works. The alternative is to reject it. Owner's
  call. The plan assumes 300 s.
- **Tray priority while paused.** Pre-pause `Pending(n)` rows show as
  `FilterOff`, because the pause is the security-relevant state. The rows
  stay in Connections.
- **`#[serde(skip)]` on a `ClientMessage` field.** It keeps the wire
  format, but it couples an internal stamp to the wire type. The alternative
  is an internal envelope on the inbound mpsc, which changes the public
  `RunningBridge::inbound_tx` type and every in-process sender. Chosen:
  `skip`.
- **File-conflict hot spots:**
  - `client_presence.rs` (`apply_pause_request`,
    `clear_pause_on_last_session_loss`) and `ws_server.rs`
    (`pump_authenticated`, `serve`);
  - bridge-cli `run_with_incoming` (pump arm, snapshot handler), with #48
    and #45;
  - `grpc_server.rs` `ask_rule`, with #44 part A;
  - `ws_messages.rs`, with #48, #44 and #45.
