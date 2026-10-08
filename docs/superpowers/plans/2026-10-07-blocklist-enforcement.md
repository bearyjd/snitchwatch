# Blocklist enforcement through opensnitchd list rules (issue #45)

**Date:** 2026-10-07
**Issue:** #45 (roadmap P0.3, the "then enforce" half; the "be honest now"
banner is the honest-ui PR)

**Blocked on:**
- draft PR #39 merging, because all wiring lives in
  `snitchwatch-bridge-cli/src/lib.rs`, which #39 rewrites;
- #48 (`2026-10-07-show-all-daemon-rules.md`), which supplies `RulesCache`,
  the "rules synced" signal and `DaemonCommands` reply correlation;
- the honest-ui PR, which adds the banner this plan removes.

**Size:** M–L. Splits cleanly into two PRs (see "Delivery").

## Owner decision (settled)

**The blocklist wins.**
- Each subscription becomes one non-precedence **deny** rule that uses
  opensnitchd's native `lists.domains` operator. The operator points at a
  list directory the bridge writes.
- In opensnitchd, a matching deny beats every non-precedence allow:
  `FindFirstMatch` keeps scanning after an allow and returns on the first
  deny, reject or precedence match (`vendor:daemon/rule/loader.go:497-515`).
- Exceptions are explicit `precedence: true` allow rules. They are out of
  scope here.

## Citation convention

- `#39:` is PR #39 at `5c2b44a`.
- `main:` is `f65a2a4`. #39 does not touch `blocklists/`,
  `translator/downstream.rs`, `ws_messages.rs` or the QML pages, so their
  lines are the same on both.
- `vendor:` is opensnitch v1.8.0.

## Goal

1. Subscribing to a blocklist blocks its hosts for every program.
2. Subscriptions survive bridge restarts and refresh on schedule.
3. The page says "enforced" only after the daemon has accepted the rule
   *and* the bridge has written the list. Every other state shows "not
   enforced" with a reason.
4. Both deployment modes work:
   - per-user `snitchwatch-bridge.service`;
   - #39's socket-activated system bridge, running as `snitchwatch`.

## Out of scope

- Exception or allow-override rules (precedence).
- Subdomain matching for ABP `||x^` entries; `lists.domains` is exact-match
  only. A `lists.domains_regexp` variant would change this.
- IPv6 literals.
- Curated presets (P5.2).
- Profiles (#46). It reuses this mechanism later.

## Findings

**Production wiring is missing entirely, not just the sink:**

- `SubscribeBlocklist`/`UnsubscribeBlocklist` reach `upstream::apply` and
  become `UpstreamEffect::None` (`#39:crates/snitchwatch-bridge/src/translator/upstream.rs:83-96`).
- `handle_blocklist_action` (`:109-125`) has no production caller; on
  #39 it appears only in a comment at `#39:lib.rs:503`.
- The blocklist-event → `SetBlocklists` broadcast pump exists only in the
  test helper `serve_with_blocklists` (`#39:ws_server.rs:322-369`).
- Production uses in-memory stores and the no-op sink
  (`#39:lib.rs:270-279`). `spawn_refresh_loop`
  (`main:blocklists/mod.rs:178-198`) has no caller.
- `refresh_now` sets `FetchStatus::Ok` (`mod.rs:144`) before the sink
  runs (`:147-153`); a sink failure is only a `warn!`.

**The current materializer makes one `dest.host` deny per entry**
(`blocklists/materializer.rs` `materialize_entry`, named
`z00-blocklist:<id>:<seq04>-<host>`). Its docs claim "user rules always
win" (`materializer.rs:1-26, 92`). The same claim appears in:
- `translator/specificity.rs:18, 42, 145`;
- `profiles/materializer.rs:7, 31`;
- `docs/superpowers/specs/2026-04-10-snitchwatch-design.md:194`.

The daemon's evaluation contradicts it, and the owner decision settles it
the other way. Rule *order* only matters for precedence rules.

**`lists.domains` semantics**
(`vendor:daemon/rule/operator_lists.go`, `operator.go:259-271`). These are
hard constraints:

- `Operator.data` is a **directory**. Every file in it merges into one map
  (`readLists`, `:227-271`). So one rule per subscription means **one
  directory per subscription**.
- Only files matching `<dir>/*.*` load, so the name needs a dot. Hidden
  files are skipped (`:25`, `:40-44`, `:241-245`).
- Only hosts-format lines load: `0.0.0.0 <host>` or `127.0.0.1 <host>`, and
  the host is taken from `line[8:]` (`filterDomains`, `:107-125`). A plain
  domain-per-line file loads nothing, silently.
- Matching is an exact map lookup on `DstHost`, lowercased when
  `sensitive: false` (`operator.go:259-271`). Write hosts in lowercase. A
  connection with an empty `DstHost` (a direct-IP connection) never
  matches.
- The daemon polls mtimes and the file count every 4 s and re-reads every
  file on change (`monitorLists`, `:17-81`).
- `Compile()` only starts that poller (`loadLists`, `:273-281`). A
  CHANGE_RULE reply of `OK` therefore does **not** prove the daemon could
  read the directory: a missing or unreadable directory loads 0 entries
  with no error back to the UI.
- On rule replace or delete, the daemon stops the old poller
  (`loader.go:310-321` `cleanListsRule`).

`lists.ips` (`readSimpleList`, one entry per line) can carry the IPv4
literals the current parser already admits. `format.rs:101-112`
`is_valid_hostname` accepts `1.2.3.4`, but such an entry can never match a
`lists.domains` lookup.

**Delivery** is the bridge's Notifications stream:
- a notification sent while no daemon holds the stream is dropped
  (`#39:lib.rs:619-628`);
- the daemon subscribes *before* it opens the stream
  (`vendor:daemon/ui/notifications.go:345-369`).

So enforcement needs a **reconcile** step on each daemon connect, gated on
#48's "rules synced" and HELLO (`stream_ready`) signals.

`derive_id` (`blocklists/mod.rs:217-241`) gives ids that collide: two URLs
ending in `/hosts` both become `hosts`, and the store upsert overwrites the
first. Production has never persisted a subscription, so changing the id
scheme now costs nothing.

## Design

### 1. State directory and list layout

The bridge resolves a state directory **only** from:
- `$STATE_DIRECTORY`, which systemd sets for both units because both
  declare `StateDirectory=snitchwatch`; or
- an explicit `SNITCHWATCH_STATE_DIR` override for development.

It canonicalizes the path, because on Bazzite `/home` is a symlink to
`/var/home`, and the rule's `data` must stay byte-stable for reconcile.
Cargo tests set neither variable, so every existing test stays in-memory
and hermetic, and none of the 23 `BridgeConfig { … }` literals change.

`run_with_incoming` takes `state_dir: Option<PathBuf>` as a parameter, the
same way `system_token_path` already is (`#39:lib.rs:231-237`):
- `run()` passes the env-derived value;
- `run_system()` additionally requires it to equal `/var/lib/snitchwatch`;
- a new `pub async fn run_with_state_dir(config, dir)` serves tests.

| | Per-user bridge | System bridge (#39) |
|---|---|---|
| Unit | `main:packaging/systemd/snitchwatch-bridge.service:55` `StateDirectory=snitchwatch` | `#39:packaging/system/snitchwatch-system-bridge.service:42-43` `StateDirectory=snitchwatch`, `StateDirectoryMode=0700` |
| Resolves to | `~/.local/state/snitchwatch` (canonical `/var/home/<u>/…` on Bazzite) | `/var/lib/snitchwatch`, owner `snitchwatch:snitchwatch`, mode 0700 |
| Writable by | the desktop user, who in this mode already controls the bridge | only `snitchwatch` (and root). `ProtectSystem=strict` still allows `StateDirectory` |
| Readable by root `opensnitchd` | yes, through DAC override; the upstream unit has no capability limits (`vendor:daemon/data/init/opensnitchd.service`) | same |
| SELinux | `~/.local/state` is labeled as a home type | `var_lib_t` |
| Unit changes | none | none; `system_package_contract.rs` stays green |

SELinux in either mode is **unverified**. Upstream ships no policy, so
opensnitchd is expected to run as `unconfined_service_t`. Check
bazzite-tower for drop-ins such as `ProtectHome=`; a VM check is required.

Layout under `<state>`:
- `blocklists.sqlite3`, mode 0600 (and later `profiles.sqlite3`, from
  #46);
- `blocklists/<id>/domains.list`, with directories at 0700 and files at
  0600;
- optionally `blocklists/<id>-ips/ips.list` (see step 3).

New module `blocklists/list_dir.rs`:
- **Write:** each write goes to `.domains.list.tmp`, which is hidden and so
  ignored by the daemon. It is then `fsync`ed and `rename`d over the target.
  The daemon never sees a truncated file, and the mtime change triggers a
  reload within 4 s.
- **Line format:** each line is exactly `0.0.0.0 <host>\n`. Hosts are
  already validated and lowercased by `format.rs`.
- **Ids:** the id must match `[A-Za-z0-9_-]+`; reject `.`, `..` and leading
  dots.
- **Remove:** `remove(id)` deletes the directory.

### 2. One rule per subscription

`materializer.rs` is rewritten as
`materialize_list_rule(id, dir) -> MaterializedRule`:
- `name`: `z00-blocklist:<id>`. Kirigami's `Rule::source()`
  (`main:crates/snitchwatch-kirigami/src/rules/row_store.rs:92-103`) still
  parses `list_id` from it.
- `action`: `deny`
- `duration`: `always`
- `precedence`: false
- `description`: the existing JSON tag, minus `entry`.
- `operator`: `{type: "lists", operand: "lists.domains", data: <canonical dir>, sensitive: false}`

Add `sensitive` to the materializer's `Operator`, and
`From<MaterializedRule> for protocol::Rule`.

Also:
- `derive_id` becomes `<sanitized stem>-<8 hex of SHA-256(url)>`;
- correct every "user rules always win" doc listed in Findings to "a
  blocklist deny beats any non-precedence allow".

### 3. Optional: IPv4 entries

Entries that parse as `Ipv4Addr` go to `<id>-ips/ips.list` and a second
rule, `z00-blocklist:<id>:ips`, with `lists.ips`. Do this only if it stays
small; otherwise defer to P5.2.

### 4. `DaemonRuleSink`

New file `blocklists/daemon_sink.rs`, implementing the existing `RuleSink`
trait (`mod.rs:53-59`) with replace semantics.

1. Write the list file(s). On failure, return
   `Err("list directory not writable: …")` and send nothing.
2. CHANGE_RULE the desired rule(s) through #48's `DaemonCommands` and wait
   5 s for the reply. Each outcome becomes an `Err` with its own reason:
   - `NoDaemon`: "daemon not connected";
   - `Rejected(text)`: the daemon's error text;
   - `Timeout`: a timeout reason.
3. DELETE_RULE every rule in #48's cache that has an owned prefix
   (`owned_blocklist_rule_name_prefixes` plus the new names) and is not in
   the desired set. This purges legacy per-entry rules from dev daemons.

Unsubscribe sends DELETE_RULE first, then removes the directory.

### 5. Honest status

`BlocklistsManager` keeps an in-memory `HashMap<id, Enforcement>`. It is
runtime state, so the SQLite schema does not change.
- `Enforcement` is `Pending`, `Enforced { at }` or `NotEnforced { reason }`,
  set from the sink result.
- The no-op sink reports `NotEnforced("no rule sink")`. This is what
  in-memory/test mode and dev runs without a state directory show.
- `BlocklistSummary` (`ws_messages.rs:434-444`) gains `enforcement` and
  `enforcement_reason`, both `#[serde(default)]`.
- `build_set_blocklists` and `build_set_blocklist_status`
  (`translator/downstream.rs:11-60`) fill them in.
- `FetchStatus` keeps meaning "download result".

### 6. Reconcile

A task waits on #48's rules-synced generation and the `stream_ready`
generation, then runs on each daemon connect:
- for each subscription with entries: rewrite the file from the store if it
  is missing; push the rule if it is missing or differs from #48's cache;
- delete owned rules whose id is no longer subscribed, which covers an
  unsubscribe made while the daemon was away;
- remove `blocklists/*` directories that have no subscription.

Rules are `always`, so the daemon persists them and keeps reading the
lists while the bridge is down.

### 7. Wiring in `run_with_incoming` (`#39:lib.rs`)

- **Stores** (`:270-279`): `BlocklistStore::open(<state>/blocklists.sqlite3)`
  when a state dir exists, otherwise in-memory as today.
  - `ProfileStore` persistence is #46's first step, not this plan's.
    Persisting profiles makes the Profiles banner's "lost when the bridge
    restarts" untrue, and its guard test asserts that wording, so that
    change belongs with the banner edit in #46.
- **Sink:** `with_rule_sink(DaemonRuleSink)`.
- **Inbound pump** (`:515-642`): route blocklist messages to
  `handle_blocklist_action` in a **spawned** task. The fetch can take up to
  `FETCH_TIMEOUT` and must not stall verdicts. Unlike profile messages
  (`:541-547`), which are awaited inline.
- **Event pump:** factor `ws_server.rs:322-369` into
  `blocklists::spawn_event_pump(mgr, broadcast_tx)` and call it from both
  places.
- **Refresh loop:** spawn `spawn_refresh_loop(15 min)` and keep its
  `JoinHandle` in `RunningBridge` so `shutdown` aborts it, like
  `watchdog_handle`.

### 8. Kirigami

- Remove the honest-ui "Preview" banner from `BlocklistsPage.qml`.
- Update its guards: `crates/snitchwatch-kirigami/tests/honest_ui_qml_guards.rs`
  `blocklists_page_warns_it_is_not_enforced`, and the Blocklists half of
  `honest_ui_pages_qml.rs` `preview_banners_are_visible_and_not_dismissable`.
  These names come from the uncommitted `fix/honest-ui` worktree; re-check
  them after it merges.
- **Per row:** show "Enforced" or "Not enforced: <reason>". The status column
  is at `BlocklistsPage.qml:101-140`.
- **Page level:** show a Warning `InlineMessage` only while some
  subscription is not enforced.
- Parse the new fields in `blocklists/row_store.rs`.
- Update `RulesPage.qml:7-11`: one row per list, not per entry.

## Delivery (two PRs)

- **PR A (S–M):** steps 1, 5 and 7, using the no-op sink. Subscriptions
  work, persist and refresh, and the page shows "not enforced: no rule
  sink". This makes the page honest even if PR B slips.
  - PR A must also reword the honest-ui Blocklists banner: subscriptions
    become persistent, so "lost when the bridge restarts" is no longer true.
  - It must relax `assert_preview_banner`'s `contains("restart")` check
    for the Blocklists page (`honest_ui_qml_guards.rs`), and keep the
    "not applied" check.
- **PR B (M):** steps 2–4, 6 and 8, after #48.

## Tests to write first

- **`list_dir`:**
  - the output parses back to the same set through a Rust port of
    `filterDomains` (test helper citing `operator_lists.go:107-125`);
  - file name contains a dot; the temp file is hidden;
  - modes are 0700 and 0600;
  - `.`/`..`/`a/b`/`.x` ids are rejected;
  - the path is canonicalized.
- **Materializer:**
  - one rule per list, named `z00-blocklist:<id>`;
  - the `lists` operator passes `mock_opensnitchd::validate_rule_shape`
    (`KNOWN_OPERATOR_TYPES` includes `"lists"`);
  - two `…/hosts` URLs get distinct ids.
- **Sink** (with a fake `DaemonCommands`):
  - OK reply means `Enforced`;
  - `NoDaemon`, `Rejected` and `Timeout` each give `NotEnforced` with a
    distinct reason;
  - a write failure sends nothing;
  - legacy `z00-blocklist:<id>:0001-x` rules in the cache are deleted.
- **Manager:**
  - enforcement is set only after the sink returns;
  - a failed refresh keeps the previous enforcement and cache;
  - a reopened file-backed store keeps subscriptions.
- **Reconcile:**
  - nothing is sent before HELLO;
  - a missing rule is pushed;
  - an orphan rule is deleted.
- **bridge-cli** (`run_with_state_dir(tempdir)`, `file://` URL of
  `tests/fixtures/blocklists/` as in `fetcher.rs` tests):
  - `SubscribeBlocklist` through `inbound_tx` broadcasts `SetBlocklists`;
  - a concurrent `SetVerdict` is processed while a slow fetch is in flight.
- **Protocol** (`bridge_protocol_test.rs`): with `MockOpensnitchd`
  subscribed and notifications open, a subscription produces one
  CHANGE_RULE with `lists.domains`. Its `data` is under the temp state dir,
  and that directory holds the hosts-format file. Replying OK gives
  `enforcement: "enforced"`.
- **Kirigami:** the row store parses `enforcement`, and the QML guard is
  updated.

## Verification

Run at low priority:

- `just test-blocklists`
- `cargo test -p snitchwatch-bridge-cli`
- `cargo test -p snitchwatch-integration-tests --test bridge_protocol`
- `cargo clippy --all-targets -- -D warnings`
- `just package-check`

Manual check in a disposable VM, in both modes:

1. Subscribe to a small hosts list.
2. `stat`/`ls -Z` the list file. Confirm
   `/etc/opensnitchd/rules/z00-blocklist:<id>.json` exists.
3. `journalctl -u opensnitchd | grep "domains loaded"` shows the count.
4. Give `curl` an "any host" allow, then `curl` a listed host: it is
   denied.
5. `ausearch -m avc -ts recent` is empty.
6. Restart the bridge: the subscription is still there.

## Risks and open questions

- **The bridge's own download goes through opensnitchd.**
  - In system mode, the `snitchwatch` account's HTTPS fetch triggers an
    AskRule. With no GUI attached it gets `Unavailable`, so the shipped
    `DefaultAction: deny` applies and the refresh fails. The previous list
    stays enforced, and the status shows the fetch error.
  - Options: a packaged allow for `process.path=/usr/bin/snitchwatch-bridge-cli`
    AND `user.name=snitchwatch`, or documenting "allow once when
    subscribing". This is part of the deny-by-default policy (Phase 1), so
    it is the owner's call.
  - On `user.name`: a `simple` `user.name` operator is resolved to a uid by
    `user.Lookup` when the rule compiles (`vendor:daemon/rule/operator.go:127-136`).
    The sysusers-assigned uid therefore doesn't need hard-coding. But a rule
    that loads before the account exists fails to compile.
- **"Enforced" is not proof of a load.** It means the daemon accepted the
  rule and the list was written, not that the daemon loaded the entries.
  Only daemon logs show the loaded count. An SELinux denial would show
  "enforced" while blocking nothing; the VM check above is the mitigation.
- **Blocklists can break the system.** Because the blocklist wins, a list
  containing a host that system services need (an rpm-ostree or flatpak
  mirror) blocks it even with a user allow. There is no exception mechanism
  until precedence rules exist.
- **Risk from a compromised bridge account.** It could point a rule's
  `data` at a special file and make root opensnitchd read `/dev/zero`,
  exhausting memory. That account already controls every AskRule answer,
  so this adds a denial-of-service vector but no new authority. Note it in
  the security review.
- **Exact-match semantics under-block ABP lists.** Show this in the UI
  copy.
- **File-conflict hot spots:**
  - `#39:lib.rs` (pump, `run_with_incoming`, snapshot) with #48, #47 and
    #46;
  - `ws_messages.rs` with #47;
  - `BlocklistsPage.qml` and its guard tests with the honest-ui PR.
