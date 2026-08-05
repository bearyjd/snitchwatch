# Plan: Warn while a pending decision may be silently exposing other traffic

## Summary

[Issue #17](https://github.com/bearyjd/snitchwatch/issues/17) and the upstream report
[evilsocket/opensnitch#1644](https://github.com/evilsocket/opensnitch/issues/1644)
establish a real, live-verified gap: `opensnitchd`'s `Client.isAsking` is a single
global flag, not per-connection. While any one `AskRule` is outstanding (up to 120s),
every *other* new connection silently gets `DefaultAction` applied — no ask, no log
above `Debug` level, invisible to all of Snitchwatch's diagnostic checks. The daemon
side of this can only be fixed upstream (`vendor/opensnitch` is a read-only submodule).
This plan is the Snitchwatch-side mitigation: since the bridge cannot observe the
silently-defaulted connections themselves (no RPC is ever made for them), the only
lever available is shrinking user response latency and making the exposure window
*visible* while it's open, via a warning banner while a decision has been pending long
enough to matter.

## User Story

As someone using Snitchwatch as my firewall's decision UI,
I want to see a clear warning when a pending Allow/Deny decision has been open long enough that other traffic may be silently passing through,
So that I know to respond promptly instead of assuming "no other prompts" means "nothing else happened."

## Problem → Solution

**Current:** A pending decision shows only as a per-row "pending" glyph
(`ConnectionsPage.qml:135-136,331,357`) and a per-group "N pending" badge
(`ConnectionsPage.qml:379-404`). Nothing indicates that *time* pending has a
consequence — a user can leave a prompt open indefinitely with no signal that doing so
has any cost beyond "that one connection is undecided."

**Desired:** Once the oldest pending row has been outstanding longer than a fixed
threshold (default: 10s — well under the daemon's 120s default-action timeout, chosen
to give a real lead-time buffer while not flashing on ordinary prompt-answering
latency), a `Kirigami.InlineMessage` warning banner appears, using the same stacking
pattern as the existing `daemonHealthBanner`, explaining that other new connections may
be getting silently allowed until the decision is answered.

## Metadata

- **Complexity**: Small–Medium (one new Rust model property + polling timer, one new
  QML banner, no protocol/wire changes, no daemon-side changes)
- **Source**: issue #17 (this repo) / evilsocket/opensnitch#1644 (upstream)
- **Estimated Files**: 3 changed (`connections_model.rs`, `main.qml`, plus a test file),
  0 new wire types

---

## UX Design

### Before

```
┌─────────────────────────────────────────────┐
│ (no chrome-level indication of elapsed time) │
│  ▸ curl (pending)                            │
│    "3 pending" badge on the group header     │
└─────────────────────────────────────────────┘
```

### After

```
┌───────────────────────────────────────────────────────────┐
│ ⚠ A decision has been pending for 14s. Until you respond,  │
│   other new connections may be silently allowed — this is  │
│   a known opensnitchd limitation, not a Snitchwatch bug.   │
├───────────────────────────────────────────────────────────┤
│  ▸ curl (pending)                                          │
│    "3 pending" badge on the group header                   │
└───────────────────────────────────────────────────────────┘
```

### Interaction Changes

| Touchpoint | Before | After | Notes |
|---|---|---|---|
| Chrome (below `daemonHealthBanner`) | No warning | `Kirigami.InlineMessage`, `type: Warning`, appears once oldest pending row's age crosses the threshold | Auto-hides once no rows are pending |
| Banner text | N/A | Static explanatory text + live-updating elapsed seconds | No "Details" action in v1 (see NOT Building) |

---

## Mandatory Reading

| Priority | File | Lines | Why |
|---|---|---|---|
| P0 | `crates/snitchwatch-kirigami/src/connections_model.rs` | 94, 331, 352, 939 | Existing `pendingCount` Qt property — the pattern to extend |
| P0 | `crates/snitchwatch-bridge/src/ws_messages.rs` | ~367 | `ConnectionRow.started_at_ms` — the only existing timestamp anchor; confirm it's set at `insert_pending` time, not connection-establish time, before relying on it as "time AskRule became pending" |
| P0 | `crates/snitchwatch-bridge/src/cache/connections.rs` | 115-124 | `insert_pending` — confirms exactly when a row becomes pending server-side |
| P0 | `crates/snitchwatch-kirigami/qml/main.qml` | 230-248 | `daemonHealthBanner` — the pattern to mirror for the new banner (model exposes `hasProblem`-style bool + summary text; banner binds to it) |
| P1 | `crates/snitchwatch-kirigami/qml/main.qml` | 212-224 | `bridgeBanner` — confirms the z-order/stacking convention for a third banner |
| P1 | `crates/snitchwatch-kirigami/qml/ConnectionsPage.qml` | 379-404 | Existing per-group "N pending" badge — do not duplicate this at the row level; the new banner is chrome-level, not per-row |
| P1 | `crates/snitchwatch-kirigami/tests/qml_source_guards.rs` | (whole file) | Existing convention for pinning QML structural invariants — the new banner's visibility condition should get a guard here, mirroring how the pending-badge fill/text fix was pinned |

## External Documentation

None needed — no new SDK/library surface, pure internal wiring.

---

## Patterns to Mirror

### BANNER_MODEL_PROPERTY (mirror `daemonHealthModel.hasProblem`/`statusSummary`)
```qml
// SOURCE: main.qml:230-248 (daemonHealthBanner)
Kirigami.InlineMessage {
    id: daemonHealthBanner
    type: Kirigami.MessageType.Warning
    visible: daemonHealthModel.hasProblem
    text: daemonHealthModel.statusSummary
    // ...
}
```

### PENDING_COUNT_PROPERTY (mirror the existing Qt property pattern)
```rust
// SOURCE: connections_model.rs:94 (pendingCount)
#[qproperty(i32, pending_count)]
```

---

## Files to Change

| File | Action | Justification |
|---|---|---|
| `crates/snitchwatch-kirigami/src/connections_model.rs` | UPDATE | Add `oldestPendingAgeSecs` (or `pendingTooLong: bool` + `pendingWarningText: String`) computed from the oldest pending row's `started_at_ms`, recomputed on a periodic tick (QML `Timer`, ~1s) rather than only on row-set changes, since elapsed time advances without any new WS message |
| `crates/snitchwatch-kirigami/qml/main.qml` | UPDATE | Add a third `Kirigami.InlineMessage` banner below `daemonHealthBanner`, bound to the new property, plus a `Timer` to force periodic re-evaluation |
| `crates/snitchwatch-kirigami/tests/qml_source_guards.rs` | UPDATE | Add a guard asserting the new banner's visibility binding stays wired to the model property (mirrors the existing pending-badge guard) |
| `HANDOFF.md` | UPDATE | Note the mitigation is live once implemented and verified |

## NOT Building

- **Any daemon-side fix.** Out of reach — `vendor/opensnitch` is read-only; tracked as
  evilsocket/opensnitch#1644 instead.
- **Detecting the actually-silently-defaulted connections.** Structurally impossible
  from the bridge's side — no RPC is ever made for them. The banner only warns about
  *exposure risk*, not observed instances.
- **A "Details" action / deep-link on the new banner.** `daemonHealthBanner` has one
  because it navigates to a whole diagnostics page; this banner has no equivalent page
  to link to in v1.
- **Configurable threshold.** Hardcode the 10s constant; revisit only if real usage
  shows it's noisy or too late.
- **Auto-responding to shrink the window (e.g. auto-deny after Ns).** A behavior
  change to verdict logic, not a warning — explicitly out of scope and a much bigger
  design conversation (would need its own plan + user sign-off given it changes what
  gets blocked/allowed).

---

## Step-by-Step Tasks

### Task 1: Confirm `started_at_ms` semantics
- **ACTION**: Before writing any code, verify (via a quick test or reading
  `insert_pending`/`connection_to_row`) whether `started_at_ms` is stamped at
  `AskRule`-received time or at earlier connection-establish time. If they can diverge
  meaningfully, decide whether to add a distinct `pending_since_ms` field instead of
  reusing `started_at_ms`.
- **VALIDATE**: A short note in the PR/commit explaining which was used and why.

### Task 2: `oldestPendingAgeSecs` (or equivalent) on `ConnectionsModel`
- **ACTION**: Add a Qt property computed from the oldest currently-pending row's
  timestamp vs. wall-clock now.
- **MIRROR**: PENDING_COUNT_PROPERTY.
- **GOTCHA**: This value changes purely with wall-clock time, not just on WS message
  arrival — the model needs an explicit re-evaluation trigger (a QML-side `Timer`
  calling an invokable, or a Rust-side ticking task) or the property will go stale
  between row-set mutations.
- **VALIDATE**: Unit test asserting the value increases across two ticks with no new
  messages, and resets to `None`/`-1` when the last pending row resolves.

### Task 3: Warning banner in `main.qml`
- **ACTION**: Add the new `Kirigami.InlineMessage`, stacked below `daemonHealthBanner`
  per the existing convention, `visible` bound to `oldestPendingAgeSecs >= 10`.
- **MIRROR**: BANNER_MODEL_PROPERTY.
- **VALIDATE**: Manual `just kirigami-dev` check — trigger a pending decision, leave it
  unanswered past 10s, confirm the banner appears and disappears on decision.

### Task 4: QML source guard
- **ACTION**: Add a guard in `tests/qml_source_guards.rs` pinning the banner's
  `visible` binding to the new property (prevents a future refactor from silently
  detaching it, matching the repo's "verify by sabotage" convention).
- **VALIDATE**: Sabotage-verify — temporarily hardcode `visible: false` and confirm the
  guard fails, then revert.

### Task 5: Update `HANDOFF.md`
- **ACTION**: Note the mitigation shipped, with the caveat that it narrows but does not
  close the exposure window (the real fix is upstream).
- **VALIDATE**: No stale claim that this "fixes" issue #17 — it mitigates.

---

## Testing Strategy

### Unit Tests
| Test | Input | Expected Output | Edge Case? |
|---|---|---|---|
| age computation, no pending rows | empty pending set | property is `None`/sentinel | No |
| age computation, one pending row | `started_at_ms` N seconds ago | returns ~N | No |
| age computation, multiple pending rows | mixed ages | returns the *oldest* row's age | Yes |
| age resets on resolution | row resolves | property returns to sentinel | Yes |

### Manual Validation (deferred to live hardware, matching this repo's convention)
- [ ] Trigger a real pending decision against the live daemon, leave it unanswered,
      confirm the banner appears at ~10s and disappears once answered

---

## Validation Commands

```bash
CCACHE_DISABLE=1 cargo clippy -p snitchwatch-kirigami --all-targets -- -D warnings
CCACHE_DISABLE=1 QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic \
  cargo test -p snitchwatch-kirigami
just qml-test
cargo fmt --all --check && git diff --check
```

---

## Acceptance Criteria
- [ ] All 5 tasks complete
- [ ] Banner appears only once oldest pending age crosses threshold, disappears when
      no rows are pending
- [ ] QML source guard sabotage-verified
- [ ] No clippy warnings, no fmt diff
- [ ] `HANDOFF.md` describes this as a mitigation, not a fix, with a link to both
      issue #17 and evilsocket/opensnitch#1644

## Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `started_at_ms` doesn't actually mean "time AskRule became pending" | Medium | Medium — banner timing would be misleading | Task 1 confirms before building on it |
| Banner noise on ordinary human response latency | Low | Low | 10s threshold chosen well above typical click latency; easy to raise if noisy |
| Users read this as "Snitchwatch bug" rather than upstream limitation | Medium | Low — reputational, not functional | Banner text explicitly attributes it as a known opensnitchd limitation |

## Notes

- This is a mitigation, not a fix. The actual fix is scoped entirely outside this
  repo's reach (`evilsocket/opensnitch#1644`). Framing this plan and the banner copy
  accordingly matters for accurately setting user expectations.
