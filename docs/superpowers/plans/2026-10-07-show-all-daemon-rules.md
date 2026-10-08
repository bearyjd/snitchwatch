# Show every daemon rule on the Rules page (issue #48)

**Date:** 2026-10-07
**Issue:** #48 (roadmap P0.2 in `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`)
**Blocked on:** draft PR #39 merging. Its pending-cancellation work rewrites
`grpc_server.rs` and the bridge-cli pump. Also blocked on the honest-ui PR
(branch `fix/honest-ui`) merging, because its `textFormat: Text.PlainText`
labels in `RulesPage.qml` must be in place before arbitrary on-disk rule
names reach that page.
**Size:** M. It is the foundation for #45 (blocklist reconcile), #46 and
the #44 second half (flagging old rules).

## Citation convention

- `#39:` is draft PR #39 at `5c2b44a`
  (`git show origin/feat/system-bridge-sockets:<path>`).
- `main:` is `f65a2a4`.
- `vendor:` is `vendor/opensnitch` v1.8.0.

Functions are named next to line numbers because the pending
pause-before-GUI fix to #39 moves `ask_rule` down by about 5 lines.

## Goal

The Rules page lists every rule opensnitchd holds, whoever created it: the
stock OpenSnitch UI, a hand-edited JSON file, an earlier bridge session, or
a blocklist. The list stays in sync with every change the bridge itself
makes, and a GUI that connects or reconnects receives the full list.

## Out of scope

- A rule editor (P2.1), rule groups (P2.5) and per-rule hit counts (P2.6).
- Learning about changes the daemon makes on its own. v1.8.0 has no
  daemon-to-UI rule push and no "list rules" action
  (`vendor:proto/ui.proto:169-194` `enum Action`). The only full snapshot is
  `ClientConfig.rules` on `Subscribe`. Edits to rule files on disk
  (`loader.go` `liveReloadWorker`) and expiry of a timed rule
  (`loader.go:434-455` `scheduleTemporaryRule`) therefore show up only on
  the next daemon (re)connect. Timed-rule expiry is approximated in step 1.
- A new `RemoveRules` ServerMessage. Deletes re-send the full `SetRules`
  instead; see step 5.

## Findings

- `#39:crates/snitchwatch-bridge/src/grpc_server.rs:624-642` (`subscribe`)
  stores only `cfg.is_firewall_running`, so `cfg.rules` is discarded. The
  daemon fills it from `c.rules.GetAll()`
  (`vendor:daemon/ui/notifications.go:35-61` `getClientConfig`), which
  includes temporary rules and disabled rules.
- The only rule broadcast is the remembered-verdict `UpdateRules` in
  `#39:grpc_server.rs:609-619` (`ask_rule`).
- `#39:crates/snitchwatch-bridge-cli/src/lib.rs:609-638` (inbound pump,
  rule effects) sends CHANGE/DELETE notifications and only logs. The
  daemon's reply is also only logged, at `#39:grpc_server.rs:714-724`
  (`notifications` reply loop). Nothing is broadcast back to the GUI after
  a toggle or delete.
  - **Verify:** whether `RulesPage.qml` flips the switch optimistically. If
    it does, the page can show a state the daemon rejected.
- `RequestSnapshot` excludes rules on purpose ("the bridge holds no rule
  cache"):
  - `#39:lib.rs:553-584`, comment at `:558-559`;
  - `main:crates/snitchwatch-bridge/src/ws_messages.rs:274-280`, which #39
    does not change.
- The Kirigami side already works:
  - `RulesStore::apply` replaces the list on `SetRules` and upserts on
    `UpdateRules` (`#39:crates/snitchwatch-kirigami/src/rules/row_store.rs:208-228`).
  - Dispatch routes both messages (`#39:crates/snitchwatch-kirigami/src/bridge_dispatch.rs:55-58`
    `interests_rules`).
  - A GUI requests a snapshot after it subscribes (`bridge_dispatch.rs:140-147`
    doc on `run_feed`).
  - The store treats index as evaluation position (`FoundRule.precedence`
    doc in `row_store.rs`). The daemon evaluates enabled rules in
    `sort.Strings` order (`vendor:daemon/rule/loader.go:368-378`
    `sortRules`), so the bridge must send rules sorted by name.
- Daemon replies:
  - The daemon replies `OK`/`ERROR` per notification id, with the error text
    in `data` (`vendor:daemon/ui/notifications.go:324-340`
    `sendNotificationReply`).
  - On stream open it sends a HELLO reply with id 0 (`:377`, `:385`).
  - Because it calls `Subscribe` *before* opening the stream
    (`:345-369`), the bridge must not push commands from inside
    `subscribe()`.

## Design

1. **`RulesCache`** (new file `crates/snitchwatch-bridge/src/cache/rules.rs`).
   - **State:** `enum RulesCache { Unknown, Synced(BTreeMap<String, Rule>) }`.
     `Unknown` means no daemon has subscribed during this bridge run; it is
     distinct from `Synced` with zero rules.
   - **Operations:**
     - `replace_all(Vec<Rule>)`
     - `upsert(Rule)`
     - `remove(&str)`
     - `snapshot_wire() -> Option<Vec<serde_json::Value>>`, which is `None`
       while `Unknown` and otherwise in name order via `rule_to_wire`
       (`#39:grpc_server.rs:35-51`, made `pub(crate)`).
     - `prune_expired(now_secs)`.
   - **Expiry rule:** a rule whose duration is not
     `once`/`until restart`/`always` (`loader.go:323-325` `isTemporary`)
     expires at `created + duration`.
     - Parse only `\d+[smh]` sequences. That covers what the bridge emits
       (`VerdictDuration::daemon_duration_str`) and the stock UI's presets.
     - A duration that fails to parse never expires in the cache.
     - This is approximate: the daemon's timer starts when it adds the rule,
       not at `created`.
2. **Ingest on `Subscribe`** (`#39:grpc_server.rs:624-642`).
   - `UiService` creates the cache internally and exposes `rules_handle()`.
     This follows the accessor pattern of `notifications_handle()` (`:313-318`)
     so the many `UiService::new` call sites stay unchanged.
   - `subscribe()` calls `replace_all(cfg.rules.clone())`, broadcasts
     `SetRules`, and bumps a `watch<u64>` "rules synced" generation, which
     #45's reconciler waits on.
   - The echoed `ClientConfig` is unchanged.
3. **Remembered verdicts** (`#39:grpc_server.rs:609-619`, `ask_rule`).
   - `upsert(rule.clone())` before the existing `UpdateRules` broadcast.
   - **Known divergence:** the daemon adds prompt replies through
     `addUserRule` → `setUniqueName` (`loader.go:380-387`, `332-342`), so it
     may store the rule as `<name>-2` while the cache holds `<name>`. #50's
     process-qualified names make this rare. The next `Subscribe` corrects
     it. Document it; don't fix it.
4. **`DaemonCommands`** (new file `crates/snitchwatch-bridge/src/daemon_commands.rs`).
   It replaces the raw `notifications_tx` plus `notification_id` pair in
   `#39:lib.rs:395-402`.
   - **Sending:** it owns the `broadcast::Sender<Notification>`, an id
     counter that starts at 1, and a `HashMap<u64, oneshot::Sender<NotificationReply>>`
     of waiters. `send(Notification) -> Result<PendingReply, NoDaemon>`.
   - **Replies:** the `notifications()` reply loop (`#39:grpc_server.rs:714-724`)
     calls `on_reply`:
     - id 0 (HELLO) bumps a `stream_ready` `watch<u64>`;
     - any other id resolves its waiter;
     - when the stream ends, every outstanding waiter fails with
       `StreamClosed`.
   - **Waiting:** `PendingReply::wait(timeout)` returns `Ok`, `Rejected(data)`,
     `Timeout` or `StreamClosed`.
   - **Who uses it:** #45 and #46 use this API to tell "enforced" from "not
     enforced".
5. **Pump rule effects** (`#39:lib.rs:609-638`).
   - The pump sends through `DaemonCommands` and spawns a waiter task (5 s
     timeout), so it never blocks the pump loop.
   - **On `Ok`:** apply the change to the cache — upsert for
     `AddRule`/`UpdateRule` (`rule_from_wire`), `remove` for `DeleteRule` —
     then broadcast the full `SetRules`. There is no remove variant, so the
     full list is simplest; rule counts are small once #45 replaces
     per-entry blocklist rules with one rule per list.
   - **On any `Err`:** log it, then re-broadcast `SetRules` from the
     unchanged cache so the page shows the daemon's real state.
   - **Validation:** a malformed rule (`notification_for_effect` returns
     `Err`) is still refused locally, as today.
6. **Snapshot** (`#39:lib.rs:553-584`).
   - When `snapshot_wire()` is `Some`, `RequestSnapshot` also sends
     `SetRules`.
   - Rewrite the comments at `#39:lib.rs:558-559` and
     `ws_messages.rs:274-280`.
7. **Expiry tick.** A 30 s interval calls `prune_expired`, and broadcasts
   `SetRules` when it removed anything.
8. **Kirigami.** No model change. Add one store test: a `SetRules` arriving
   after `UpdateRules` replaces the list, so a reconnect snapshot drops
   rules that were deleted elsewhere.

## Tests to write first

Bridge unit tests (`cache/rules.rs`, `daemon_commands.rs`,
`grpc_server/tests.rs`):

- `Unknown` yields no snapshot. `Synced(empty)` yields an empty `SetRules`.
- `replace_all` output is name-sorted.
- `rule_to_wire` → `rule_from_wire` keeps `precedence`/`nolog` and `list`
  operators.
- A `"5m"` rule created 301 s ago is pruned. `"always"`, `"until restart"`
  and an unparseable duration are not.
- `on_reply`:
  - an OK reply resolves only its own id;
  - an ERROR reply surfaces the text from `data`;
  - id 0 bumps `stream_ready` without resolving anything;
  - stream close fails pending waiters;
  - `send` with no daemon returns `NoDaemon`.
- `subscribe` with three rules broadcasts one `SetRules` holding all
  three, in name order.
- A remembered `ask_rule` verdict upserts the cache. A `Once` verdict does
  not.

Protocol test (`tests/bridge_protocol_test.rs`, modelled on #39's
`rule_update_and_delete_reach_the_daemon_as_notifications` at `:576`):

1. `MockOpensnitchd::subscribe_with_config` with two pre-existing rules.
   An authenticated WS client sends `RequestSnapshot` and receives
   `SetRules` with both.
2. The mock opens notifications.
3. The client sends `UpdateRule` (disable).
4. The mock replies OK with the same id. The client then receives
   `SetRules` showing the rule disabled.
5. Repeat with a `DeleteRule` and an ERROR reply. The client receives
   `SetRules` with the rule unchanged.

`MockOpensnitchd::open_notifications` already hands back a reply sender
(`tests/mock_opensnitchd/src/lib.rs` `open_notifications`).

Kirigami unit test: the `RulesStore` replacement test in step 8.

## Verification

Run these at low priority (`nice -n 19`), because another session runs
timing-sensitive VM tests on this host:

- `cargo test -p snitchwatch-bridge`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo clippy -p snitchwatch-bridge -p snitchwatch-bridge-cli --all-targets -- -D warnings`
- `cargo test -p snitchwatch-kirigami rules::`, with
  `QT_QPA_PLATFORM=offscreen`.

Manual VM check: rules created by the stock UI or written to
`/etc/opensnitchd/rules/` appear after a bridge restart. Toggling one in
Snitchwatch flips its JSON `enabled` field on disk.

## Risks and open questions

- **Staleness is inherent.** Disk edits and daemon-side expiry are only
  seen on reconnect. Consider showing "as of last daemon connection" in a
  later UI pass.
- **Thousands of rules.** A full `SetRules` on every change is
  O(rules). Pre-#45 per-entry blocklist rules could reach tens of
  thousands, but none were ever installed in production (the sink is a
  no-op). If a daemon does hold them, #45's reconcile deletes them.
- **Multiple daemon streams.** These are normally one. Waiters resolve on
  the first reply. Uncertain; no test models two daemons.
- **File-conflict hot spots** with #47 and #45:
  - `#39:lib.rs` snapshot handler and pump;
  - `grpc_server.rs` (`subscribe`, `notifications`, `ask_rule`).
