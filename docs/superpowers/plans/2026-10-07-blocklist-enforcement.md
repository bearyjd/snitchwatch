# Blocklist enforcement through opensnitchd list rules (issue #45)

**Date:** 2026-10-07 (revised after #39 merged as `670f42c`)
**Issue:** #45 (roadmap P0.3, the "then enforce" half; the "be honest now"
banner is the honest-ui PR)
**Baseline:** `main` @ `670f42c`, which includes #39, the four pause fixes
and #50.
**Blocked on:**
- PR A: the honest-ui PR, which adds the banner and the guard tests that
  PR A rewords;
- PR B: #48 (`2026-10-07-show-all-daemon-rules.md`), for `RulesCache`, the
  "rules synced" signal and `DaemonCommands` correlation tied to the
  current daemon stream.

**Size:** PR A S–M, then PR B M.

## Owner decision (settled)

**The blocklist wins.**
- Each subscription becomes one non-precedence **deny** rule that uses
  opensnitchd's native `lists.domains` operator. The operator points at a
  list directory the bridge writes.
- In opensnitchd, a matching deny beats every non-precedence allow:
  `FindFirstMatch` keeps scanning after an allow and returns on the first
  deny, reject or precedence match (`vendor:daemon/rule/loader.go`
  `FindFirstMatch`, ~497-515).
- Exceptions are explicit `precedence: true` allows. They are out of scope.

## Citation convention

- `main:` means `670f42c`. Functions and tests are cited by name. Line
  numbers are approximate; go by the name.
- `vendor:` means opensnitch v1.8.0.

## Goal

1. Subscribing to a blocklist installs a daemon rule that blocks its hosts
   for every program.
2. Subscriptions persist and refresh on schedule, with a bounded, safe
   fetcher.
3. The page says "rule installed" only after the daemon accepted the rule
   *and* the bridge wrote the list. Every other state shows "not enforced"
   with a reason.
4. Both deployment modes work: the per-user `snitchwatch-bridge.service`,
   and the system bridge running as `snitchwatch`.

## Out of scope

- Precedence exceptions.
- ABP subdomain semantics. `lists.domains` is exact-match.
- IPv6 literals.
- Curated presets (P5.2).
- Profiles (#46).
- Any URL scheme other than `https`, in production.

## Findings (`main`)

**Production wiring is missing entirely:**
- `SubscribeBlocklist`/`UnsubscribeBlocklist` reach
  `translator/upstream.rs` `apply` and become `UpstreamEffect::None`.
- `handle_blocklist_action` has no production caller; the only mention in
  bridge-cli is a comment.
- The event → `SetBlocklists` broadcast pump exists only in the test helper
  `ws_server::serve_with_blocklists`.
- `run_with_incoming` builds both stores in-memory
  (`BlocklistStore::open_in_memory`) with the no-op sink.
  `BlocklistsManager::spawn_refresh_loop` has no caller.
- `BlocklistsManager::refresh_now` sets `FetchStatus::Ok` before the sink
  runs; a sink failure is a `warn!`.

**SECURITY: the fetcher is unsafe for user-supplied URLs**
(`blocklists/fetcher.rs` `fetch`):
- `file://<path>` is read with `tokio::fs::read_to_string` and **no size
  cap**.
- `http://` is accepted, and nothing restricts redirect schemes.
- The 64 MiB cap is checked on `Content-Length` before the read, but
  `resp.text()` buffers a chunked body of any size before the post-read
  check.

In system mode, any `snitchwatch-ui` member can send `SubscribeBlocklist`.
A `file:///dev/zero` subscription would OOM the bridge. Once PR A makes
subscriptions persistent and starts the refresh loop, the bridge would OOM
again on **every restart**.

**The materializer** makes one `dest.host` deny per entry
(`blocklists/materializer.rs` `materialize_entry`, named
`z00-blocklist:<id>:<seq04>-<host>`). It claims "user rules always win".
The same claim appears in `translator/specificity.rs`,
`profiles/materializer.rs` and `docs/superpowers/specs/2026-04-10-snitchwatch-design.md`
(the "locked to the 900–999 band" sentence). opensnitchd's evaluation and
the owner decision contradict it.

**`lists.domains` semantics** (`vendor:daemon/rule/operator_lists.go`;
`operator.go` `domainsListsCmp`). These are hard constraints:

- `data` is a **directory**. Every file in it merges into one map
  (`readLists`), so one rule per subscription means one directory per
  subscription.
- Only files matching `<dir>/*.*` load (a dot is required). Hidden files
  are skipped (`monitorLists`, `readLists`).
- Only `0.0.0.0 <host>` / `127.0.0.1 <host>` lines load; the host is
  `line[8:]` (`filterDomains`). A plain domain-per-line file loads nothing,
  silently.
- Matching is an exact lookup on the lowercased `DstHost`. Direct-IP
  connections (empty `DstHost`) never match.
- The daemon polls every 4 s and re-reads every file on any change
  (`monitorLists`).
- `Compile()` only starts that poller (`loadLists`). A CHANGE_RULE `OK`
  therefore does **not** prove the daemon could read the directory: under
  `ProtectHome=`, SELinux or a wrong path it loads 0 entries with no error
  back.

**Other facts:**
- `format.rs` `is_valid_hostname` admits IPv4 literals, which can never
  match `lists.domains`. They would fit `lists.ips` (`readSimpleList`).
- `blocklists/mod.rs` `derive_id` collides: two URLs ending in `/hosts`
  both get id `hosts`. Nothing has ever been persisted, so fix it before
  PR A starts persisting ids.
- Notifications sent while no daemon holds the stream are dropped. The
  daemon calls `Subscribe` before it opens the stream
  (`vendor:daemon/ui/notifications.go` `Client.Subscribe`). So PR B needs a
  reconcile step gated on #48's signals.

## Design

### PR A: wire, persist, refresh, be honest (no daemon rules yet)

**A1. A safe fetcher.**
- `build_client()` sets `https_only(true)` and
  `redirect::Policy::limited(5)`. **Verify with a test** that reqwest also
  rejects an `https → http` redirect under `https_only`.
- `fetch` drops the `file://` branch. Production accepts only `https`.
- Read the body in a streaming loop (`resp.chunk()`) with a running
  `MAX_BODY_BYTES` cap, so a chunked or endless body stops at 64 MiB.
- At subscribe time, `handle_blocklist_action` validates the URL with
  `url::Url`: scheme `https`, a host present, length ≤ 2048. A bad URL
  becomes a visible `Failed { reason }` and is never stored.

**A2. A test-only fetch hook** usable from the `tests/` crates, which
`#[cfg(test)]` doesn't reach.
- `pub trait BlocklistFetch: Send + Sync { async fn fetch(&self, url) -> FetchOutcome }`.
  The default implementation is the HTTPS fetcher.
- Inject it with `BlocklistsManager::with_fetcher(Arc<dyn BlocklistFetch>)`,
  and through bridge-cli with `run_with_options(config, RunOptions { state_dir, blocklist_fetcher })`.
  `main.rs` never passes a fetcher.
- The fixture fetcher (file-backed, size-capped) lives **in the test
  files**, so production code has no file-reading fetch path at all.
- Migrate `fetcher.rs` `parses_local_fixture_via_file_url` and
  `crates/snitchwatch-bridge/tests/blocklists_e2e.rs` to it.
- Alternative if trait injection proves awkward: a non-default
  `test-fetch` Cargo feature, enabled only in dev-dependencies, plus a
  release-verify check that the shipped binary was built without it.

**A3. One worker task.**
- `run_with_incoming` spawns a single blocklist worker that owns an mpsc
  queue of jobs: `Subscribe(url)`, `Unsubscribe(id)`, `RefreshDue`.
- The pump only enqueues. It never awaits a fetch, which can take up to
  `FETCH_TIMEOUT`, so verdicts are never stalled.
- `spawn_refresh_loop` enqueues `RefreshDue` instead of fetching itself.
- Effect: jobs run in order, with at most one 64 MiB fetch at a time.
- Keep the worker's and the refresh loop's `JoinHandle`s in
  `RunningBridge` so `shutdown` aborts them, like `watchdog_handle`.

**A4. Stable ids.** `derive_id` becomes
`<sanitized stem>-<8 hex of SHA-256(url)>`, in this PR because this PR
starts persisting ids.

**A5. State directory.** It is resolved only in `main.rs`/`run_system`.
`run()` and the in-process shells never read it, so every test that calls
`run()` stays in-memory and hermetic whatever the developer's environment.
- `main.rs` resolves `$STATE_DIRECTORY` (systemd sets it for both units,
  each declaring `StateDirectory=snitchwatch`), then a
  `SNITCHWATCH_STATE_DIR` override. It canonicalizes the path, because
  `/home` is a symlink to `/var/home` on Bazzite and the rule `data` must
  be byte-stable. It then calls `run_with_options`.
- `run_system` requires the resolved path to equal `/var/lib/snitchwatch`.
- Tests that spawn the binary as a subprocess set `STATE_DIRECTORY` and
  `SNITCHWATCH_STATE_DIR` to a tempdir with `Command::env`.
- A source-guard test asserts that only `main.rs` and `run_system` call the
  resolver.
- None of the 23 `BridgeConfig { … }` literals change.

| | Per-user bridge | System bridge |
|---|---|---|
| Unit | `packaging/systemd/snitchwatch-bridge.service` `StateDirectory=snitchwatch` | `packaging/system/snitchwatch-system-bridge.service` `StateDirectory=snitchwatch`, `StateDirectoryMode=0700` |
| Resolves to | `~/.local/state/snitchwatch` (canonical `/var/home/<u>/…`) | `/var/lib/snitchwatch`, `snitchwatch:snitchwatch`, 0700 |
| Writable by | the desktop user, who already controls this bridge | only `snitchwatch` (and root). `ProtectSystem=strict` keeps `StateDirectory` writable, so no unit change and `system_package_contract.rs` stays green |
| Read by root `opensnitchd` | DAC override; the upstream unit has no capability limits (`vendor:daemon/data/init/opensnitchd.service`) | same |

SELinux is **unverified**, and so is any `ProtectHome=`/`ProtectSystem=`
drop-in that bazzite-tower adds to opensnitchd. Either would make the
daemon load 0 entries silently. This is the main reason the success state
is only "rule installed" (A6).

**A6. Persistence and honest status.**
- `BlocklistStore::open(<state>/blocklists.sqlite3)` (mode 0600) when a
  state dir exists; in-memory otherwise.
- `BlocklistsManager` keeps an in-memory map from id to `Enforcement`:
  - `Pending`;
  - `RuleInstalled { at }`: the daemon replied OK and the list file was
    written. The UI label is **"Rule installed"**, *not* "Enforced", because
    the daemon may still load 0 entries;
  - `NotEnforced { reason }`.
- Until PR B, the no-op sink reports `NotEnforced("no rule sink yet")`.
- `BlocklistSummary` (`ws_messages.rs`) gains `enforcement` and
  `enforcement_reason`, both `#[serde(default)]`. `translator/downstream.rs`
  `build_set_blocklists`/`build_set_blocklist_status` fill them in.
- `FetchStatus` keeps meaning "download result".

**A7. Wiring.**
- The pump routes blocklist messages to the A3 worker.
- Factor `serve_with_blocklists`' event pump into
  `blocklists::spawn_event_pump(mgr, broadcast_tx)` and call it from both
  places.

**A8. GUI.**
- Subscriptions now survive restarts. Reword the honest-ui Blocklists
  banner: still "not applied" (until PR B), but no longer "lost when …
  restarts".
- Relax that page's `contains("restart")` check in
  `honest_ui_qml_guards.rs` `assert_preview_banner`. Keep "not applied"
  and the no-"bridge"-word rule.
- Show the per-row enforcement state (`blocklists/row_store.rs`,
  `BlocklistsPage.qml`).

### PR B: install the rules (after #48)

**B1. List directory** (`blocklists/list_dir.rs`).
- Layout: `<state>/blocklists/<id>/domains.list`, with directories at 0700
  and files at 0600.
- Write to `.domains.list.tmp`, `fsync`, then `rename`. The temp file is
  hidden, so the daemon ignores it.
- Each line is exactly `0.0.0.0 <host>\n`.
- Ids must match `[A-Za-z0-9_-]+`; reject leading dots.
- Optional: IPv4 entries go to `<id>-ips/ips.list` with a second rule,
  `z00-blocklist:<id>:ips` using `lists.ips`.

**B2. One rule per subscription.** `materialize_list_rule(id, dir)`:
- `name`: `z00-blocklist:<id>`. Kirigami `Rule::source()` still parses
  `list_id` from it.
- `action`: `deny`
- `duration`: `always`
- `precedence`: false
- `operator`: `{type: "lists", operand: "lists.domains", data: <canonical dir>, sensitive: false}`

Also add `sensitive` to the materializer's `Operator`, and
`From<MaterializedRule> for protocol::Rule`. Correct every "user rules
always win" doc listed in Findings.

**B3. `DaemonRuleSink`** (`blocklists/daemon_sink.rs`, implementing
`RuleSink` with replace semantics):
1. Write the file(s). On failure, return `NotEnforced` and send nothing.
2. CHANGE_RULE the rule(s) through #48's `DaemonCommands` and wait 5 s.
   Each outcome maps to its own reason: `NoDaemon`, `Rejected(text)`,
   `Timeout`, `StreamClosed`.
3. DELETE_RULE every cached rule that has an owned prefix and is not
   desired. This purges legacy per-entry rules.

Unsubscribe sends DELETE first, then removes the directory.

**B4. Reconcile.** It runs on each daemon connect, after #48's
rules-synced and `stream_ready` signals, as a job on the A3 worker.
- For each subscription with entries: rewrite the file if it is missing,
  and push the rule if it is missing or differs.
- Delete owned rules for ids no longer subscribed.
- Remove orphan directories.

**B5. GUI.**
- Remove the Blocklists banner and its guard assertions.
- Show a page-level warning only while some subscription is not enforced.
- Update `RulesPage.qml`'s header comment (one row per list).

## Tests to write first

**PR A:**
- **Fetcher:**
  - `http://` and `file://` are rejected;
  - an `https` → `http` redirect fails (local TLS test server, or
    `wiremock`/`httpmock` if one is already in the dependency tree — check);
  - a chunked body over 64 MiB stops at the cap without buffering more;
  - a bad URL is never stored.
- **Worker:**
  - two `SubscribeBlocklist` jobs are processed in order;
  - a `SetVerdict` sent while a slow fake fetch is in flight is applied
    immediately;
  - only one fetch is ever concurrent (the fake fetcher counts).
- **`derive_id`:** two `…/hosts` URLs get distinct, stable ids.
- **Persistence:** with `run_with_options(state_dir: tempdir, fixture fetcher)`,
  subscribe, shut down and restart; the subscription is still there and the
  refresh loop schedules it.
- **Hermetic:**
  - a source-guard test asserts that `run()`/`run_with_incoming` never read
    `STATE_DIRECTORY`/`SNITCHWATCH_STATE_DIR`, and that only `main.rs` and
    `run_system` call the resolver;
  - a subprocess test of the binary with a tempdir passed through
    `Command::env` writes `blocklists.sqlite3` only there. The subprocess
    must also get an isolated `XDG_RUNTIME_DIR` and
    `SNITCHWATCH_GRPC_BIND=127.0.0.1:0`; per CLAUDE.md, a bridge started
    without them replaces the running bridge's socket and token.
  - Never use `std::env::set_var` in-process.
- **Status:** `enforcement: "not_enforced"` with reason "no rule sink yet".
  The Kirigami row store parses it, and the QML guard is updated.

**PR B:**
- **`list_dir`:**
  - round-trips through a Rust port of `filterDomains` (test helper citing
    `operator_lists.go` `filterDomains`);
  - a dotted file name; a hidden temp file;
  - modes 0700/0600;
  - bad ids rejected;
  - a canonical path.
- **Rule shape:** passes `mock_opensnitchd::validate_rule_shape`
  (`"lists"` is a known type).
- **Sink** (with a fake `DaemonCommands`):
  - OK gives `RuleInstalled`;
  - every failure gives `NotEnforced` with a distinct reason;
  - a write failure sends nothing;
  - legacy rules are deleted.
- **Reconcile:** nothing is sent before HELLO; a missing rule is pushed; an
  orphan is deleted.
- **Protocol** (`bridge_protocol_test.rs`): the mock subscribes and opens
  notifications. A subscription produces one CHANGE_RULE with
  `lists.domains`, `data` under the temp state dir, and a hosts-format file
  there. Replying OK gives `enforcement: "rule_installed"`.

## Verification

Run at low priority:

- `just test-blocklists`
- `cargo test -p snitchwatch-bridge-cli`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo clippy --all-targets -- -D warnings`
- `just package-check`

Manual check in a disposable VM, in both modes:

1. Subscribing to `file:///dev/zero` is refused.
2. Subscribe to a small list. Check its `stat`/`ls -Z`, and that
   `/etc/opensnitchd/rules/z00-blocklist:<id>.json` exists.
3. `journalctl -u opensnitchd | grep "domains loaded"` shows the expected
   count. This is the only proof that entries actually loaded.
4. Give `curl` an "any host" allow, then `curl` a listed host: denied.
5. `ausearch -m avc` is empty.
6. Restart the bridge: the subscription persists.

## Risks and open questions

- **The bridge's own download goes through opensnitchd.**
  - In system mode with no GUI attached, the `snitchwatch` account's HTTPS
    fetch gets `Unavailable`, the shipped deny default applies, and the
    refresh fails. The previous list stays installed, and the status shows
    the fetch error.
  - Options: a packaged allow for
    `process.path=/usr/bin/snitchwatch-bridge-cli` AND `user.name=snitchwatch`,
    or "allow once when subscribing". This belongs to the Phase 1
    deny-by-default policy, so it is the owner's call.
  - A `simple` `user.name` operator is resolved to a uid by `user.Lookup`
    when the rule compiles (`vendor:daemon/rule/operator.go` `Compile`). So
    the sysusers uid needn't be hard-coded, but a rule loaded before the
    account exists fails to compile.
- **"Rule installed" is not proof of a load.** Only daemon logs show the
  count. The VM check is the mitigation; surfacing the count needs a daemon
  change.
- **Blocklists can break the system.** Because the blocklist wins, a list
  entry that system services need is blocked even with a user allow, and
  there is no exception mechanism yet.
- **Internal fetches from a privileged account.** With `https` only, a
  `snitchwatch-ui` member can still make the `snitchwatch` account fetch
  internal `https` endpoints, and hostname-like tokens from the response
  show up as entries. Consider rejecting loopback and private-range
  targets. Owner's call.
- **Exact-match semantics under-block ABP lists.** Say so in the UI copy.
- **File-conflict hot spots:**
  - bridge-cli `run_with_incoming` and the pump, with #48, #47 and #46;
  - `ws_messages.rs` and `translator/downstream.rs` (PR A's summary
    fields), with #47, #48 and #44;
  - `BlocklistsPage.qml` and its guards, with the honest-ui PR.
