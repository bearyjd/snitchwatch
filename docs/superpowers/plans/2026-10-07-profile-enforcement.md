# Profiles: persistence now, enforcement later (issue #46)

**Date:** 2026-10-07 (revised after #39 merged as `670f42c`)
**Issue:** #46 (roadmap P0.4, the "enforce later" half; the "be honest
now" banner is the honest-ui PR)
**Baseline:** `main` @ `670f42c`.
**Blocked on:**
- Part 1: #45 PR A, for state-dir resolution and `run_with_options`;
- Part 2: #45 PR B and #48, for `DaemonCommands`, `RulesCache` and
  reconcile.

**Status (2026-10-08):** Part 1 is implemented on
`fix/46-profile-persistence` (based on `c74d8c7`, after #45 PR B). Part 2
is not started. See "Part 1 as built" below.

**Priority: lowest of the post-#39 set.** No GUI control can create a
profile rule, so there is nothing to enforce yet. Persistence is cheap.
Enforcement is worth doing together with the rule editor (P2.1).

**Size:** Part 1 S. Part 2 M, plus the GUI editor.

## Citation convention

`main:` means `670f42c`. Functions are cited by name. Line numbers are
approximate; go by the name.

## Goal

1. **Part 1:** profiles and the active-profile choice survive bridge
   restarts. The Profiles page says exactly what is and isn't true.
2. **Part 2:** activating a profile installs exactly that profile's rules
   and removes the previous profile's rules. Status is reported the honest
   way #45 does it.

## Out of scope

- A profile-rule editor. `ClientMessage::AddProfileRule`/`RemoveProfileRule`
  and `ProfileRuleWire` exist in `ws_messages.rs`, but nothing in
  `crates/snitchwatch-kirigami` sends them. That work belongs with P2.1.
- Changing the network-matcher or auto-switch semantics (`profiles/mod.rs`
  module doc).

## Findings (`main`)

- **No persistence, no-op sink:**
  - `run_with_incoming` uses `ProfileStore::open_in_memory()`;
  - the default sink is `NoopProfileRuleSink` (`profiles/mod.rs`).
- **Sink failures are only logged.** In `activate_inner`, `rematerialize`
  and `delete_profile`, a sink failure is just a `warn!`.
- **Materialized shape** (`profiles/materializer.rs` `materialize_rule`):
  - name: `850-profile:<id>:<seq04>-<rule_id>`;
  - operator: always `simple`, with the client-supplied `operand`/`data`,
    neither validated;
  - duration: `always`.
- **Case-folding bug.** The materializer's `Operator` struct has **no
  `sensitive` field**, so every operator goes out `sensitive: false`. For
  `process.path`, that reintroduces the case-folding bug #50 fixed: a
  non-sensitive simple compare uses `strings.EqualFold`, so `/tmp/ſcript`
  matches `/tmp/script`.
- **Empty `data` is dangerous.** An empty-`data` `dest.host` operator
  matches every connection with an empty `DstHost` (direct-IP traffic).
- **The ordering doc is wrong.** `profiles/materializer.rs` says profile
  overrides "always win over blocklist denies" because of sort order. Under
  opensnitchd's `FindFirstMatch` and the blocklist-wins decision (#45), a
  non-precedence profile **allow** loses to any matching deny.
  *Already corrected on `main` by #45 PR B (`c74d8c7`): the module doc and
  `PROFILE_BAND_PREFIX` now say a profile allow does not beat a blocklist
  deny. Part 1 left it as is.*

## Design

### Part 1: persistence and honest banner (S, right after #45 PR A)

1. **Store.** In `run_with_incoming`, with #45's `RunOptions.storage =
   Persistent(dir)`, use `ProfileStore::open(<dir>/profiles.sqlite3)`
   (mode 0600). Otherwise use in-memory.
   - Apply #45's failure policy: an open failure becomes
     `Ephemeral(Unusable("profile store: <err>"))`, logged and surfaced, and
     the store falls back to in-memory.
   - The profile store's mode is tracked separately from the blocklist
     store's.
2. **Same signal as #45.** `ServerMessage::SetProfiles` gains
   `#[serde(default)] storage: Option<StorageStatus>` (#45's type), filled
   in by `translator/downstream.rs` `build_set_profiles`.
3. **Banner keyed on it.** Reword the honest-ui Profiles banner:
   - **`persistent: true`:** "…not applied to the firewall yet…" only.
   - **otherwise (or `None` from an older bridge):** keep the "kept in
     memory only… lost when Snitchwatch's background service restarts"
     sentence, plus the reason for `Unusable`.
   - Update `honest_ui_qml_guards.rs` `assert_preview_banner` for this page
     the same way as #45 does for Blocklists:
     - the non-persistent variant still contains "restart";
     - both contain "not applied";
     - `visible: true`, non-dismissable, and no word "bridge".

### Part 1 as built (2026-10-08)

Differences from the design above, and why:

- **Store hardening.** `ProfileStore::open` now does what
  `BlocklistStore::open` does: `O_NOFOLLOW` pre-open, regular file only,
  mode 0600 (tightened if it exists), `SQLITE_OPEN_NOFOLLOW`, and a
  `PRAGMA user_version` (1) that refuses a newer schema. Duplicated rather
  than shared, so #45's store is untouched.
- **Where it lives.** `snitchwatch-bridge-cli/src/profile_storage.rs`
  (`build_profiles_manager`), not `lib.rs`. `run_with_incoming` clones
  `options.storage` before the blocklist builder consumes `options`. No
  `BridgeMode` gate: per-user bridges save profiles too, since nothing is
  installed.
- **Unreadable store.** A `profiles.sqlite3` that opens but whose profiles
  can't be read is treated like one that can't be opened:
  `Unusable("profile store: …")` and memory, with the file left as is. The
  blocklist store keeps an unreadable store (`StorageStatus.unreadable`) to
  protect installed rules; profiles have none, and `build_set_profiles`
  reads the store directly, so keeping it would hide every profile.
- **Banner.** Two fixed-text warnings instead of one banner with two
  wordings, because `inline_messages_carry_only_fixed_text` (#51) forbids
  conditional InlineMessage text:
  - `visible: true`: "not applied to the firewall", "no firewall rules";
  - `visible: !page.storagePersistent`: memory only, lost on restart;
  - plus a fixed-text note, `visible: page.storagePersistent`, saying
    profiles are saved (not in the design: the page has to say both
    states), and the reason in a PlainText "Details:" label.
  `assert_preview_banner` is replaced by `assert_profiles_banners`.

### Part 2: enforcement (M, after #45 PR B and P2.1's editor)

4. **`DaemonProfileSink`**, implementing `ProfileRuleSink` on #48's
   `DaemonCommands` with replace semantics:
   - CHANGE_RULE every desired rule;
   - DELETE_RULE every cached `850-profile:<id>:` rule that is not desired;
   - wait 5 s for the replies and return `Err(reason)` on `NoDaemon`,
     `Rejected`, `Timeout` or `StreamClosed`.
5. **Materializer fixes.**
   - Add `sensitive` to the materializer's `Operator` and **force
     `sensitive: true` for `process.path`**, matching #50's
     `process_path_operator`.
   - Leave other operands `false`. `dest.host` lowercases, which is
     correct.
6. **Validation**, in `handle_profile_action` or `ProfilesManager::add_rule`:
   - `operand` must be in the daemon's known operand list. Mirror
     `tests/mock_opensnitchd` `KNOWN_OPERANDS`, minus `list` and `lists.*`,
     because the type is fixed at `simple`;
   - `data` must be non-empty;
   - `action` must be `allow` or `deny`.
   Reject anything else.
7. **Honest status.** Record an `Enforcement` for the active profile, using
   the same enum and "Rule installed" label as #45. `SetProfiles` gains
   `enforcement` and `enforcement_reason`, both `#[serde(default)]`. The
   Active chip reads "Active — not enforced: <reason>" on failure.
8. **Reconcile.** Reuse #45's reconcile job: the daemon holds exactly the
   active profile's rules, and any other `850-profile:` rule is deleted.
9. **Rules page.** Add `RuleSource::Profile { profile_id }` for the
   `850-profile:` prefix (Kirigami `rules/row_store.rs` `Rule::source`).
10. **Banner.** Remove it only when Part 2 *and* a profile-rule editor ship.
11. Fix the doc comments listed under Findings.

## Tests to write first

**Part 1:**
- a file-backed `ProfileStore` reopened in a tempdir keeps profiles and the
  active flag;
- bridge-cli `run_with_options(config, RunOptions { storage: Persistent(tempdir), .. })`
  → `CreateProfile` → shutdown → restart in the same directory →
  `RequestSnapshot` → `SetProfiles` still lists it, with
  `storage.persistent == true`;
- `run()` (in-process) → `SetProfiles.storage` is not persistent;
- an unopenable `profiles.sqlite3` → `Unusable("profile store…")`, the
  bridge still starts, and the banner keeps the restart sentence;
- the QML guard is updated.

**Part 2:**
- **Materializer:** a `process.path` rule materializes with
  `sensitive: true`, and `dest.host` stays `false`.
- **Validation:**
  - empty `data` is rejected;
  - `lists.domains` is rejected;
  - an unknown operand is rejected;
  - a valid `process.path` rule passes `mock_opensnitchd::validate_rule_shape`.
- **Sink:**
  - activating B after A deletes A's rules and installs B's;
  - each failure mode gives `NotEnforced(reason)`.
- **Reconcile:** a stray `850-profile:old:0000-x` is deleted after HELLO.

## Verification

Run at low priority:

- `cargo test -p snitchwatch-bridge profiles`
- `cargo test -p snitchwatch-bridge-cli`
- `cargo test -p snitchwatch-kirigami` with `QT_QPA_PLATFORM=offscreen`
- `cargo clippy --all-targets -- -D warnings`

Manual VM check for Part 2:
1. Activate a profile with one deny rule: the
   `/etc/opensnitchd/rules/850-profile:…json` file exists and blocks.
2. Switch profiles: the old file is gone.

## Risks and open questions

- **Precedence (owner decision needed, still open after Part 1).** Should a
  profile **allow** override blocklist and prompt denies? That needs
  `precedence: true`, which also beats the user's own denies. The plan keeps
  `precedence: false` (the materializer emits no precedence at all),
  consistent with "blocklist wins", and only fixes the docs. Part 1 does not
  touch the materializer; decide before Part 2.
- **A saved manual pick vs. auto-switch at startup (open).** The active
  profile now survives a restart, but the auto-switch loop's first network
  observation after a start counts as a network change: if a *different*
  profile's matchers match the current network, it is activated, replacing
  a manual pick saved before the restart. The pin itself is not persisted.
  Harmless while nothing is enforced; decide with Part 2.
- **Rule churn.** Auto-switch on NetworkManager changes now rewrites
  firewall rules. Debounce flapping networks in Part 2.
- **Is Part 2 worth doing before P2.1?** It would ship an enforcement path
  no GUI can exercise. Recommendation: land Part 1 now and schedule Part 2
  with the editor.
- **File-conflict hot spots:**
  - bridge-cli `run_with_incoming`, with #45;
  - `ProfilesPage.qml` and the guard tests, with the honest-ui PR.
