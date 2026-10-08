# Show every daemon rule on the Rules page (issue #48)

**Date:** 2026-10-07 (revised after #39 merged as `670f42c`)
**Issue:** #48 (roadmap P0.2 in `docs/superpowers/specs/2026-10-07-competitive-feature-roadmap.md`)
**Baseline:** `main` @ `670f42c`, which includes #39, the four pause fixes
and #50.
**Also blocked on:** the honest-ui PR (branch `fix/honest-ui`) merging. Its
`textFormat: Text.PlainText` labels in `RulesPage.qml` must be in place
before arbitrary on-disk rule names reach that page.
**Size:** M. It is the foundation for #45 PR B, #46 Part 2 and #44 Part B.

## Citation convention

- `main:` means `670f42c`. Functions and tests are cited by name. Line
  numbers are approximate; go by the name.
- `vendor:` means `vendor/opensnitch` v1.8.0.

## Goal

The Rules page lists every rule opensnitchd holds, whoever created it: the
stock OpenSnitch UI, a hand-edited JSON file, an earlier bridge session, or
a blocklist. The list stays in sync with every change the bridge itself
makes, and a GUI that connects or reconnects receives the full list.

## Out of scope

- A rule editor (P2.1), rule groups (P2.5) and per-rule hit counts (P2.6).
- Learning about changes the daemon makes on its own. v1.8.0 has no
  daemon-to-UI rule push and no "list rules" action
  (`vendor:proto/ui.proto` `enum Action`). The only full snapshot is
  `ClientConfig.rules` on `Subscribe`. Disk edits (`loader.go`
  `liveReloadWorker`) and timed-rule expiry (`loader.go`
  `scheduleTemporaryRule`) show up on the next daemon (re)connect. Expiry
  is approximated in step 1.
- A new `RemoveRules` ServerMessage. Deletes re-send the full `SetRules`.
- Closing daemon impersonation in legacy (TCP loopback) mode. That is
  issue #35. Step 4 keeps today's command fan-out, so the real daemon
  still gets every command. A local impostor can still spoof the Rules list
  and reply status (see Risks).

## Findings (`main`)

- `grpc_server.rs` `subscribe` stores only `cfg.is_firewall_running`, so
  `cfg.rules` is discarded. The daemon fills it from `c.rules.GetAll()`
  (`vendor:daemon/ui/notifications.go` `getClientConfig`), which includes
  temporary and disabled rules.
- The only rule broadcast is the remembered-verdict `UpdateRules` in
  `ask_rule`. Its log line is "persistent verdict rule broadcast failed".
- The inbound pump's rule-effect arm (bridge-cli `run_with_incoming`,
  `notification_for_effect`) sends CHANGE/DELETE notifications through the
  raw `notifications_tx` and only logs.
  - The daemon's replies are only logged too (`notifications` reply loop,
    "notification reply from daemon").
  - Nothing is broadcast back after a toggle or delete. **Verify** whether
    `RulesPage.qml` flips the switch optimistically.
- `RequestSnapshot` excludes rules on purpose. See the comment in the
  pump's `SnapshotRequested` arm ("Rules are excluded — the bridge holds no
  rule cache") and the `ClientMessage::RequestSnapshot` doc in
  `ws_messages.rs`.
- Kirigami already handles the messages:
  - `RulesStore::apply` (`rules/row_store.rs`) replaces on `SetRules` and
    upserts on `UpdateRules`;
  - `bridge_dispatch::interests_rules` routes both;
  - `run_feed` requests a snapshot after subscribing.
  - The store treats index as evaluation position (the `FoundRule` doc).
    The daemon evaluates enabled rules in `sort.Strings` order
    (`vendor:daemon/rule/loader.go` `sortRules`), so send rules sorted by
    name.
- **Daemon replies:**
  - OK/ERROR per notification id, with the error text in `data`
    (`notifications.go` `sendNotificationReply`);
  - a HELLO with id 0 when the stream opens (`listenForNotifications`).
  - The daemon calls `Subscribe` **before** opening the stream
    (`Client.Subscribe`), so the bridge must not push commands from inside
    `subscribe()`.
- **Toggled temporary rules.**
  - `rule_from_wire` sets `created: 0`.
  - On a CHANGE_RULE, `replaceUserRule` schedules a *new* timer, but the
    daemon's *original* timer still fires on the original schedule: in
    `scheduleTemporaryRule`'s callback, the duration is unchanged, so it
    deletes the rule. A toggled 5-minute rule therefore vanishes at its
    original `created + 5m`.
  - The cache must keep the original `created` across a toggle.
- **Impersonation.**
  - Legacy mode listens on TCP `127.0.0.1:50051`, so any local user can
    call `Subscribe` or open a `Notifications` stream and reply to
    commands.
  - System mode accepts only root peers (`RootUnixIncoming` in bridge-cli).
  - Outbound notifications fan out to *every* open stream
    (`UiService::notifications` subscribes each stream to the same
    broadcast).

## Design

1. **`RulesCache`** (new file `crates/snitchwatch-bridge/src/cache/rules.rs`).
   - **State:** `enum RulesCache { Unknown, Synced(BTreeMap<String, Rule>) }`.
     `Unknown` means no daemon has subscribed during this bridge run; it is
     distinct from `Synced` with zero rules.
   - **Operations:**
     - `replace_all(Vec<Rule>)`
     - `upsert(Rule)`: when an entry exists and the incoming `created` is
       0, it **keeps the cached `created`**. Toggles go through
       `rule_from_wire`, which zeroes it.
     - `remove(&str)`
     - `snapshot_wire() -> Option<Vec<Value>>`, which is `None` while
       `Unknown`, in name order, via `rule_to_wire` (`grpc_server.rs`, made
       `pub(crate)`)
     - `prune_expired(now_secs)`
   - **Expiry rule:** a rule whose duration is not
     `once`/`until restart`/`always` (`loader.go` `isTemporary`) expires at
     `created + duration`.
     - Parse `\d+[smh]` sequences only.
     - A duration that fails to parse, or `created == 0`, never expires.
     - This is approximate.
2. **Ingest on `Subscribe`** (`grpc_server.rs` `subscribe`).
   - `UiService` creates the cache internally and exposes `rules_handle()`
     (the accessor pattern of `notifications_handle`).
   - `subscribe()` does **not** write the cache. It stores `cfg.rules` in
     a bounded **pending-snapshot set** keyed by
     `ConnKey = Option<SocketAddr>` from `request.remote_addr()`:
     - TCP (legacy mode): `Some(peer addr)`.
     - `None`: Unix sockets (system mode, where tonic's `UdsConnectInfo`
       has no `remote_addr`) and `Request::new` unit tests. `None` is
       **one shared key**. In system mode every peer is root
       (`RootUnixIncoming`), so sharing is fine.
   - **Bounds.** A unary `Subscribe` has no connection-close hook, so
     stale entries can't be dropped on close. Instead:
     - each key keeps only its latest snapshot;
     - the set holds at most 4 keys, evicting the oldest;
     - an entry older than 30 s is ignored and removed at commit time.
   - **Commit on HELLO** (step 4). When a HELLO on stream N (connection key
     K) makes N current:
     1. set `current_stream = N` and bump `stream_ready`;
     2. **then**, if a fresh pending snapshot exists for K, remove it from
        the set, `replace_all(snapshot)`, broadcast `SetRules`, and bump the
        "rules synced" generation.

     A HELLO with no pending snapshot only makes its stream current and
     leaves the cache untouched. Because `current_stream` is set before the
     `SetRules` broadcast, a client that has seen `SetRules` can send rule
     commands without hitting `NoDaemon`.
   - **Why commit on HELLO.** The daemon always calls `Subscribe` on a new
     connection *before* its stream sends HELLO (`Client.Subscribe` →
     `listenForNotifications`). An old stream may also stay open until the
     10 s HTTP/2 keepalive notices it's dead. So a redial's snapshot must
     be held until its own HELLO arrives.
   - The echoed `ClientConfig` is unchanged.
3. **Remembered verdicts** (`ask_rule`).
   - `upsert(rule.clone())` before the existing `UpdateRules` broadcast.
   - **Known divergence:** the daemon adds prompt replies through
     `addUserRule` → `setUniqueName`, so it may store `<name>-2`. #50's
     process-qualified names make this rare. The next `Subscribe` corrects
     it. Document it.
4. **`DaemonCommands`** (new file `crates/snitchwatch-bridge/src/daemon_commands.rs`).
   It replaces the raw `notifications_tx` and `notification_id` plumbing in
   `run_with_incoming`.
   - **Stream identity:** each `notifications()` call gets a stream id from
     a counter, and records its `ConnKey` (`request.remote_addr()`).
     `DaemonCommands` keeps the set of **open** streams, each with the
     sequence number of its HELLO, if it has sent one.
     - **Current stream:** the open stream with the newest HELLO. A HELLO
       (id 0) on stream N makes N current (step 2).
     - **When the current stream closes,** the current stream falls back to
       the newest *still-open* stream that sent a HELLO, or to none. The
       fallback commits no snapshot and leaves the cache as it is.
   - **Outbound, by transport** (`UiService::with_daemon_transport(…)`, set
     by `run` and `run_system`):
     - **TCP (legacy per-user mode):** commands keep **fanning out to every
       open stream**, as today. The real daemon always receives them even
       if another local process has opened a stream. No regression.
     - **Root-only Unix socket (system mode):** commands go only to the
       current stream. Every peer is root, so there is no impersonation to
       defend against.
   - **Replies (both modes):** a non-zero reply resolves its waiter only if
     it arrives on the stream that is current **at reply time**. Other
     streams' replies are logged and ignored. In TCP mode waiters are not
     tied to a stream; in Unix mode a reply must also come from the stream
     the command went to.
   - **When streams close:**
     - **TCP mode:** in-flight waiters stay pending while any stream
       remains open; fan-out already delivered their command to every
       stream. They fail with `StreamClosed` only when no stream is left.
     - **Unix mode:** the command went only to the old current stream, so
       its in-flight waiters fail with `StreamClosed` when that stream
       closes.
   - **Snapshot source:** only the current stream's connection can commit a
     snapshot (step 2).
   - **What this does and doesn't buy in TCP mode.** A fake local
     "daemon" cannot stop the real daemon from receiving commands:
     - fan-out still delivers every command to every open stream;
     - `send` returns `NoDaemon` only when *no* stream is open;
     - a fake that sends a HELLO and then closes just hands "current" back
       to the real daemon's still-open stream.

     But while a fake that subscribed and sent a *later* HELLO is the
     current stream, it can:
     - replace the Rules list shown to the user;
     - answer replies, faking "rule installed" for #45/#46;
     - get its forged rule bodies installed by the real daemon: a toggle
       the user makes on one of its rows is sent, body and all, to every
       stream.

     Its list is withdrawn (cache `Unknown`, empty `SetRules`) as soon as
     its stream closes or stops being current (review finding H1), so it
     cannot outlive the fake. This is a residual risk until the TCP
     transport is retired (#35). See Risks.
   - **Mock change (required):** `MockOpensnitchd::open_notifications`
     must send `NotificationReply { id: 0, code: OK }` first, as the real
     daemon's `listenForNotifications` does.
   - **Readiness for tests.** HELLO is handled asynchronously, so a test that
     sends `UpdateRule` right after `open_notifications()` returns could
     race it and get `NoDaemon`. This affects
     `rule_update_and_delete_reach_the_daemon_as_notifications`.
     - Add a **`pub`** accessor, `RunningBridge::daemon_stream_ready() -> watch::Receiver<u64>`.
       It must not be `#[cfg(test)]`: the `tests/` crate can't see those.
     - Tests wait with a **level check**, for example
       `ready.wait_for(|g| *g >= 1).await`, not "await a change". A plain
       `changed()` hangs if the HELLO was already handled before the test
       subscribed.
     - Protocol tests that open mock notifications and must stay green:
       - `idle_daemon_with_open_notifications_stream_stays_reachable`;
       - `notifications_stream_close_triggers_down_transition_within_one_tick`;
       - `rule_update_and_delete_reach_the_daemon_as_notifications`, which
         must await readiness.
   - **API:**
     - `send(Notification) -> Result<PendingReply, NoDaemon>`. `NoDaemon`
       means **no open stream at all** in TCP mode, and no current stream
       in Unix mode;
     - `PendingReply::wait(timeout)` returns `Ok`, `Rejected(data)`,
       `Timeout` or `StreamClosed`.
   - #45 and #46 use this to report whether a rule is installed.
5. **Pump rule effects.** In the inbound pump's rule-effect arm, send
   through `DaemonCommands` and spawn a waiter task (5 s timeout) so the pump
   never blocks.
   - **On `Ok`:** apply the change to the cache — upsert (keeping `created`)
     for `AddRule`/`UpdateRule`, `remove` for `DeleteRule` — then broadcast
     the full `SetRules`.
   - **On any `Err`:** log it, then re-broadcast `SetRules` from the
     unchanged cache.
6. **Snapshot.** The `SnapshotRequested` arm also sends `SetRules` when
   `snapshot_wire()` is `Some`. Rewrite its "Rules are excluded" comment and
   the `RequestSnapshot` doc in `ws_messages.rs`.
7. **Expiry tick.** A 30 s interval calls `prune_expired` and broadcasts
   `SetRules` if anything was removed.
8. **Kirigami.** No model change. Add one store test: a `SetRules` after an
   `UpdateRules` replaces the list.

## Tests to write first

**Bridge unit tests** (`cache/rules.rs`, `daemon_commands.rs`,
`grpc_server/tests.rs`):

- `Unknown` yields no snapshot. `Synced(empty)` yields an empty
  `SetRules`. `replace_all` output is name-sorted.
- `rule_to_wire` → `rule_from_wire` keeps `precedence`/`nolog` and `list`
  operators.
- **Expiry and `created`:**
  - a `"5m"` rule with `created = now - 301` is pruned;
  - `always`, `until restart`, unparseable durations and `created == 0` are
    not;
  - **toggled temporary rule:** cache a `"5m"` rule with `created = T`,
    upsert it through `rule_from_wire(rule_to_wire(..))` (which zeroes
    `created`), advance 31 s, run `prune_expired`. The rule is still listed
    and its `created` is still `T`. At `T + 301` it is pruned.
- **Replies and streams:**
  - an OK reply resolves only its own id;
  - ERROR surfaces the text in `data`;
  - a HELLO on stream 2 makes it current, and a later OK for a pending id
    arriving on stream 1 is ignored (that waiter times out);
  - **TCP transport:** a command reaches both open streams (fan-out kept);
  - **Unix transport:** a command reaches only the current stream;
  - **TCP fallback, new command:**
    1. S1 sends a HELLO, then S2 sends a HELLO (S2 is current);
    2. S2 closes;
    3. a new command still reaches S1, and S1's OK resolves it.
  - **TCP fallback, in-flight command:** a command sent while S2 is current
    is still resolved by S1's OK after S2 closes.
  - **TCP, last stream:** when the last open stream closes, in-flight
    waiters fail with `StreamClosed`, and the next `send` returns
    `NoDaemon`.
  - **TCP, no HELLO yet:** with a stream open that has not sent a HELLO,
    `send` fans out and does not return `NoDaemon`.
  - **Unix:** closing the current stream fails its waiters, and `send`
    with no current stream returns `NoDaemon`.
- **`subscribe` and commit on HELLO:**
  - with three rules, a `Subscribe` followed by a HELLO from the same
    connection broadcasts one name-sorted `SetRules`; "rules synced" moves
    only at the HELLO, and `current_stream` is already set when `SetRules`
    is sent;
  - after a commit, the snapshot has been removed from the pending set;
  - a HELLO with no pending snapshot makes its stream current and leaves the
    cache and the "rules synced" generation untouched;
  - pending-set bounds: a fifth key evicts the oldest; a 31 s old snapshot
    is not committed;
  - `Request::new` (no `remote_addr`) uses the shared `None` key;
  - a `Subscribe` from a connection that never sends a HELLO never replaces
    the cache.
- **Redial with a stale stream** (server-level test with **two real tonic
  channels**, i.e. two `MockOpensnitchd::connect` instances, so
  `remote_addr` differs):
  1. channel 1 subscribes and opens notifications; its stream stays open;
  2. channel 2 subscribes with different rules, then opens notifications;
  3. channel 2's snapshot is adopted, and its replies are the ones
     correlated.
- **`ask_rule`:** a remembered verdict upserts. `Once` does not.

**Protocol test** (`tests/bridge_protocol_test.rs`, modelled on
`rule_update_and_delete_reach_the_daemon_as_notifications`):

1. `MockOpensnitchd::subscribe_with_config` with two rules.
2. An authenticated WS client sends `RequestSnapshot`: no `SetRules` yet,
   because the cache is still `Unknown`.
3. The mock opens notifications. With this plan's mock change, that sends
   HELLO, so the client receives `SetRules` with both rules.
4. Wait on `RunningBridge::daemon_stream_ready()` with
   `wait_for(|g| *g >= 1)`.
5. `UpdateRule` (disable) with the mock replying OK gives `SetRules` with
   the rule disabled.
6. `DeleteRule` with an ERROR reply gives `SetRules` unchanged.

**Kirigami:** the `RulesStore` replacement test.

## Verification

Run at low priority (`nice -n 19`), because another session runs
timing-sensitive VM tests on this host:

- `cargo test -p snitchwatch-bridge`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo clippy -p snitchwatch-bridge -p snitchwatch-bridge-cli --all-targets -- -D warnings`
- `cargo test -p snitchwatch-kirigami rules`, with
  `QT_QPA_PLATFORM=offscreen`

Manual VM check:

1. Rules from the stock UI, or written to `/etc/opensnitchd/rules/`, appear
   after a bridge restart.
2. Toggling one flips its JSON `enabled` on disk.
3. A toggled 5-minute rule disappears at its original expiry.

## Risks and open questions

- **Staleness is inherent.** Disk edits and daemon-side expiry are seen
  only on reconnect.
- **Large rule sets.** A full `SetRules` is O(rules). Pre-#45 per-entry
  blocklist rules were never installed in production (no-op sink). #45's
  reconcile deletes any that a dev daemon holds.
- **Residual impersonation risk in legacy TCP mode** (until #35 retires
  the TCP transport for the per-user bridge).
  - Any local user can connect to `127.0.0.1:50051`, call `Subscribe` and
    send a later HELLO. **While their stream is the current one**, they
    can:
    - replace the Rules list the GUI shows;
    - answer command replies, so #45/#46 report "rule installed" for a
      rule the real daemon may have rejected;
    - have the real daemon install a forged rule body: a toggle the user
      makes on one of the forged rows goes, body and all, to every stream.
  - *(Corrected after review: an earlier version said "while that
    connection stays open", and the forged list then outlived the
    impostor. The list now belongs to the stream that committed it and is
    withdrawn when that stream closes or another stream becomes current
    without its own snapshot.)*
  - The real daemon still receives every command. TCP mode keeps the
    fan-out, `NoDaemon` requires zero open streams, and when the impostor
    disconnects, the current stream falls back to the daemon's still-open
    stream. The impostor cannot cut the daemon off.
  - The number of open daemon streams is not capped: on TCP a cap would
    let a local process fill it and lock the real daemon's stream out.
  - Today that same user can already send fake `AskRule` prompts and fake
    stats on that port (#35). This adds rule-list and status spoofing
    to that existing exposure.
  - System mode (root-only Unix socket) is not affected.
  - **A `security-reviewer` pass on `DaemonCommands`, stream correlation
    and the pending-snapshot set is required before merge.**
  - *(Orchestrator decision, owner may revisit: no regression of TCP
    fan-out.)*
- **File-conflict hot spots:**
  - bridge-cli `run_with_incoming` (pump, snapshot), with #47 and #45;
  - `grpc_server.rs` (`subscribe`, `notifications`, `ask_rule`), with #47
    and #44.
