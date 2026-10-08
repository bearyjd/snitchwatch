# Profiles: persistence now, enforcement later (issue #46)

**Date:** 2026-10-07
**Issue:** #46 (roadmap P0.4, the "enforce later" half; the "be honest
now" banner is the honest-ui PR)
**Blocked on:**
- #39 merging (the wiring lives in `snitchwatch-bridge-cli/src/lib.rs`);
- #45 PR A, which adds state-dir resolution and `run_with_state_dir`;
- for enforcement only, #45 PR B and #48 (`DaemonCommands`, `RulesCache`,
  reconcile).

**Priority: lowest of the post-#39 set.** Profiles currently have nothing
to enforce, because no GUI control can create a profile rule (see
Findings). Persistence is cheap and worth doing soon. Enforcement only
becomes valuable once a profile-rule editor exists, which needs the rule
editor (P2.1).

**Size:**
- Part 1 (persistence and honest banner): S.
- Part 2 (enforcement): M, plus the GUI editor work it depends on.

## Citation convention

- `#39:` is PR #39 at `5c2b44a`.
- `main:` is `f65a2a4`. #39 does not touch `profiles/`, `ws_messages.rs`
  or `ProfilesPage.qml`.

## Goal

1. **Part 1:** profiles and the active-profile choice survive bridge
   restarts. The Profiles page says exactly what is and isn't true.
2. **Part 2:** activating a profile installs exactly that profile's rules
   in opensnitchd and removes the previous profile's rules. Status is
   reported the same honest way as for blocklists.

## Out of scope

- A profile-rule editor in Kirigami. `AddProfileRule`/`RemoveProfileRule`
  exist in the protocol (`main:ws_messages.rs:264-271`, `ProfileRuleWire`
  at `:459-464`), but nothing in `crates/snitchwatch-kirigami` sends them.
  This work belongs with P2.1.
- Changing the network-matcher or auto-switch semantics
  (`profiles/mod.rs` module doc).

## Findings

- **No persistence, no-op sink:**
  - production uses `ProfileStore::open_in_memory()` (`#39:lib.rs:276-279`);
  - the default sink is `NoopProfileRuleSink` (`main:profiles/mod.rs:66-77`).
- **Sink failures are only logged.** In `activate_inner` (`mod.rs:224-254`),
  `rematerialize` and `delete_profile`, a sink failure is just a `warn!`.
- **Materialized shape** (`profiles/materializer.rs` `materialize_rule`):
  - name: `850-profile:<id>:<seq04>-<rule_id>`;
  - operator: always `simple`, with the *client-supplied* `operand` and
    `data`, neither validated;
  - duration: `always`.
- **Empty `data` is dangerous.** A `dest.host` operator with empty `data`
  would match every connection with an empty `DstHost` (direct-IP traffic),
  so a profile deny of that shape blocks broadly.
- **The ordering doc is wrong.** `profiles/materializer.rs:1-16, 31` says
  profile overrides "always win over blocklist denies" because of sort
  order. Under opensnitchd's evaluation (`vendor:daemon/rule/loader.go:497-515`)
  and owner decision 1 for #45, a non-precedence profile **allow** loses to
  any matching deny, blocklist or otherwise. Only `precedence: true` would
  let it win.

## Design

### Part 1: persistence and honest banner (S, can land right after #45 PR A)

1. In `run_with_incoming`, use `ProfileStore::open(<state>/profiles.sqlite3)`
   (mode 0600) when #45's resolved state dir exists. Fall back to
   in-memory otherwise, which covers tests and dev.
2. Reword the honest-ui banner in `ProfilesPage.qml`. Profiles are now
   saved but still "not applied to the firewall".
   - The banner on the uncommitted `fix/honest-ui` worktree currently has
     `showCloseButton: true`, which contradicts its own guard. Re-check
     that after the PR merges.
   - Update `honest_ui_qml_guards.rs` `profiles_page_warns_it_is_not_enforced`
     so it still requires "not applied" but no longer requires "restart".

### Part 2: enforcement (M, after #45 PR B and P2.1's editor)

3. **`DaemonProfileSink`**, implementing `ProfileRuleSink` (`mod.rs:56-63`)
   on #48's `DaemonCommands` with replace semantics:
   - CHANGE_RULE every desired rule;
   - DELETE_RULE every cached `850-profile:<id>:` rule that is not desired;
   - wait for the replies (5 s) and return `Err` with a reason on
     `NoDaemon`, `Rejected` or `Timeout`.
4. **Validation**, at `handle_profile_action` (`#39:upstream.rs:141-196`) or
   in `ProfilesManager::add_rule`:
   - `operand` must be in the daemon's known operand list. Mirror
     `tests/mock_opensnitchd/src/lib.rs` `KNOWN_OPERANDS`, and exclude
     `lists.*` and `list`, because the type is fixed at `simple`;
   - `data` must be non-empty;
   - `action` must be `allow` or `deny`.
   Reject anything else instead of materializing it.
5. **Honest status.** `ProfilesManager` records an `Enforcement` for the
   active profile, using the same enum as #45. `SetProfiles` gains
   `enforcement` and `enforcement_reason`, both `#[serde(default)]`. When
   the sink fails, the "Active" chip on `ProfilesPage.qml` reads "Active —
   not enforced: <reason>".
6. **Reconcile.** Reuse #45's reconcile task: on each daemon connect, make
   the daemon hold exactly the active profile's rules, and delete any other
   `850-profile:` rules.
7. **Rules page.** Add `RuleSource::Profile { profile_id }` for the
   `850-profile:` prefix (`main:crates/snitchwatch-kirigami/src/rules/row_store.rs:92-103`)
   so these rules group like blocklist rules.
8. **Banner.** Remove the Profiles banner only when Part 2 ships *and* a
   profile-rule editor exists.
9. Fix the doc comments listed under Findings.

## Tests to write first

**Part 1:**
- a file-backed `ProfileStore` reopened in a tempdir keeps profiles and the
  active flag;
- bridge-cli `run_with_state_dir(tempdir)` → `CreateProfile` → shutdown →
  restart in the same directory → `RequestSnapshot` → `SetProfiles` still
  contains the profile;
- the QML guard is updated.

**Part 2:**
- **Sink** (with a fake `DaemonCommands`):
  - activating B after A deletes A's rules and installs B's;
  - each failure mode gives `NotEnforced(reason)`.
- **Validation:**
  - empty `data` is rejected;
  - `lists.domains` is rejected;
  - an unknown operand is rejected;
  - `process.path` with data is accepted, and
    `mock_opensnitchd::validate_rule_shape` passes on the materialized rule.
- **Reconcile:** a stray `850-profile:old:0000-x` is deleted, and the
  active profile's rules are pushed after HELLO.

## Verification

Run at low priority:

- `cargo test -p snitchwatch-bridge profiles`
- `cargo test -p snitchwatch-bridge-cli`
- `cargo test -p snitchwatch-kirigami` with `QT_QPA_PLATFORM=offscreen`
- `cargo clippy --all-targets -- -D warnings`

Manual VM check for Part 2: activate a profile with one deny rule, and
confirm `/etc/opensnitchd/rules/850-profile:…json` exists and blocks.
Switch profiles and confirm the old file is gone.

## Risks and open questions

- **Precedence (owner decision needed).** Should a profile **allow**
  override blocklist and prompt denies? That needs `precedence: true`,
  which also beats the user's own denies, a much stronger power. The plan
  keeps `precedence: false`, consistent with "blocklist wins", and only
  corrects the docs.
- **Auto-switch on network change** (`spawn_auto_switch`) means
  NetworkManager events now rewrite firewall rules. A flapping network
  could churn the rules. Consider debouncing in Part 2.
- **Is Part 2 worth doing before P2.1?** Doing it earlier ships an
  enforcement path that no GUI can exercise. Recommendation: land Part 1
  now and schedule Part 2 with the rule editor.
- **File-conflict hot spots:**
  - `#39:lib.rs` `run_with_incoming`, with #45;
  - `ProfilesPage.qml` and the guard tests, with the honest-ui PR.
