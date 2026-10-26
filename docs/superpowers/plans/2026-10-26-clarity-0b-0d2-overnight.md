# Clarity phase 0: verdict counts (0b) and held-back notice count (0d part 2)

Status: approved by the owner 2026-10-26 ("go" on P01 + C03, GUI-only overnight
run). Parent: `2026-10-22-gui-phase0-clarity.md` sections 0b and 0d (DEFERRED
bullet). Roadmap: `../specs/2026-10-24-competitive-feature-roadmap.md` P01, C03.
GUI-only (`crates/snitchwatch-kirigami`); no wire, bridge, image, unit or daemon
config change. If either item needs one, stop and re-plan.

Not in this run: 0c (exception-first default). It hides rows by default and
parent open question 3 (hide or only collapse the background group) is
unanswered; the owner confirms that default first.

## Shared rules
- The phase-0 safety invariant holds: no row that needs a human is hidden,
  collapsed or suppressed; neither item changes visibility at all.
- Role numbers are fixed so the two PRs don't collide: 0d2 takes
  `ROLE_RETRIED_COUNT = 41`; 0b takes 42 and up.
- Files at or near 800 lines are split before they grow
  (`connections/grouping.rs` 772, `connections_model.rs` 737,
  `connections_model/apply.rs` 590).
- TDD: each new behaviour gets a failing test first. Kirigami tests run
  headless (`QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`), and the
  local `cargo test -p snitchwatch-kirigami` plus clippy `-D warnings` gate is
  mandatory; `just kirigami-screenshots` before/after goes on each PR.
- Review: independent `/codex` adversarial pass, fix all, merge on green CI.

## PR A: 0d part 2, "notice held back N times"
Design is the parent's DEFERRED bullet, unchanged:
- `retries: Arc<Mutex<HashMap<FlowKey, u32>>>` in `BridgeHandles` (GUI-local).
  The controller writes it when `may_post` returns `Repeat`, reading
  `CooldownGate::suppressed` (`notifier.rs:183`) without holding the handles lock
  while it holds the gate lock, and removes the entry when the flow is cleared.
- One shared function derives (session, program, host) for both `flow_key()`
  and the model side; test the `process_path` present and absent shapes.
- Role `retriedCount` is non-zero only on the newest pending row of that flow
  (greatest `started_at_ms`), new code in `src/connections_model/retries.rs`,
  refreshed on the existing 1 s pending-age timer.
- Label: "notice held back N times in the last hour", separate from the
  collapse `x N` text; never "retried".
- Tests: counter increments only on Repeat; cleared on Action, ServerChanged,
  show/bus failure and a guard dropped with no outcome; only the newest pending
  row carries it; a held-back notice never removes the row or the banner; every
  literal `BridgeHandles` fixture gains the field.

## PR B: 0b, per-group verdict counts with a time window
- First extract `GroupCounts` (and its update logic) out of `grouping.rs` into
  `connections/grouping/counts.rs`, behaviour unchanged (existing tests green).
- `RowMeta` gains `started_at_ms`; windowed counts for "last hour" and "today"
  (local midnight), each split into allowed by rule, allowed by default,
  denied by rule, denied by default, blocklisted, pending. `decided_by_default`
  (or no matched rule with an action) decides "by default".
- Windows need a clock: recompute on a coarse tick (60 s) with an injected
  clock in tests, emitting `dataChanged` only for group rows whose figures moved.
- Delegate: icon plus word per verdict (never colour alone), e.g.
  "Last hour: 12 allowed (3 by default) · 2 denied · 1 pending".
- Honesty: figures cover only rows the GUI holds. Verify whether the bridge
  replays cached rows on connect (`ClearConnectionRows` then replay) and word
  the tooltip from that result ("limited to the rows the bridge replays" or
  "empty after a restart"). Never claim a true hour.
- Tests: incremental counts across insert/update/evict, window rollover with an
  injected clock, by-rule vs by-default split, reconnect replay,
  `tests/connections_model_qml.rs` role exposure, delegate text.

## After both merge
Rebuild the GUI Flatpak from the merged main with the reviewed one-shot
wrapper, install it locally, record SHA-256 in `.omc/STATE.md`, and send the
tower the merged SHA. No host staging or reboot.
