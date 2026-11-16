# #363: headless Kirigami keyboard probes are timing-fragile under load

Issue #363 named two flakes from PR #362's CI. PR #365 (`a2ae174a`) fixed one race (the status-pages
Continue step asserted the saved mode before the async save landed). Owner chose (2026-11-15) to
hunt the rest. The hunt shows the problem is the whole family, not two tests.

## Evidence (reproduction recipe)

Run each `tests/*keyboard*.rs` test binary in a loop while `3 x nproc` `yes >/dev/null` loops run
(`QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`). No load: 25/25 green. 2 busy loops:
green. 66 busy loops, first 6 of 15 rounds: failures in `profiles`, `connections`, `blocklists`,
`answer_surfaces`, `map`, `recommended`. Failure shapes seen:

1. `never ready for <step>` (the step engine's stall budget: `maxStalls` 200 ticks of 60-100 ms,
   so roughly 12-20 s of wall clock, runs out on a loaded box).
2. Tab itinerary differs from the expected one (sheet/page not settled when `itinerary()` /
   `tabTo()` ran; `QQuickRootItem` / `ScrollView` stops, `Close` after a later control).
3. `cannot Tab to <control>` / `could not focus <control>` (focus move before the control exists
   or is enabled).
4. A signal-vs-state race like the one #365 fixed (assert right after a trigger).

Caveat: 66 loops is far harsher than a CI runner. This proves fragility, not the CI failure rate
(3 flakes seen on #362). Acceptance below uses the same recipe, so it is stricter than CI needs.

## Design (shared helpers only, no per-test patches)

Steps run in `tests/support/list_keys.qmlpart` (list pages) and in per-probe copies of the same
engine (`status_pages_probe.qml`, `connections_*`, `answer_surfaces_*`, `map_*`; verify by grep
before editing). Fix once per engine:

1. **Wall-clock stall budget.** Replace the tick count with elapsed time (e.g. 60 s) so a slow
   box is not penalised; keep the failure message naming the awaited thing.
2. **Settle before Tab.** `itinerary()` / `tabTo()` / `mustTab()` first wait for the mounted page
   or sheet to hold still (the existing `stable()` check in the status probe; port it to
   `list_keys.qmlpart`) and for `activeFocus` to be committed, instead of sending keys at once.
3. **Diagnostics on failure.** On any probe failure print the focus item, the Tab order seen, and
   the last 20 stderr lines (the geography test already prints its itinerary).
4. Gate every "assert after trigger" on the effect, not the signal (the #365 pattern); fix any
   additional instance the hunt finds.

Not changing: what the probes assert, QML under `qml/`, or production code. If a failure turns out
to be a real product focus bug (the control really is unreachable when idle), stop and file it
separately instead of papering over it in the probe.

## Steps (TDD where it applies)

1. Finish the hunt; collect the full failure list (`scratchpad/hunt.log`) and classify each by
   shapes 1-4. Every shape must map to a helper change above or be listed as out of scope.
2. Change the helpers; keep each engine's change a mechanical copy.
3. Acceptance: the same hunt, 15 rounds x all keyboard probes at 66 loops, 0 failures; also one
   run with no load. Mutation check: temporarily make `stable()` return true immediately and
   confirm the hunt fails again (proves the settle wait is what fixes it).
4. PR, independent opus review plus `/codex` adversarial pass, merge on green CI, close #363 with
   the hunt numbers.

## Risks

- Settle waits lengthen the suite (kirigami job is already about 17 min). Measure; cap the wait.
- A larger stall budget can hide a genuinely hung probe for longer: keep a hard overall cap.
