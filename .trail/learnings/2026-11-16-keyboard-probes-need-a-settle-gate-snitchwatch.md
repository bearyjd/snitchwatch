---
title: Keyboard probes need a settle gate and watchdogs longer than their stall budget
author: Snitchwatch
date: 2026-11-16
branch: fix/363-probe-settle
cost: { minutes: 30, tokens: 19197 }
id: l-526b5b83
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-kirigami/tests/rules_keyboard_qml.rs
  - crates/snitchwatch-kirigami/tests/support/probe_runner.rs
  - crates/snitchwatch-kirigami/tests/alert_window_keyboard_qml.rs
  - crates/snitchwatch-kirigami/tests/answer_surfaces_keyboard_qml.rs
  - crates/snitchwatch-kirigami/tests/connections_accessible_names_qml.rs
  - crates/snitchwatch-kirigami/tests/connections_keyboard_qml.rs
  - crates/snitchwatch-kirigami/tests/connections_row_click_qml.rs
  - crates/snitchwatch-kirigami/tests/probes/learning_keyboard_phases.qmlpart
  - crates/snitchwatch-kirigami/tests/probes/learning_keyboard_probe.qml
  - crates/snitchwatch-kirigami/tests/probes/map_keyboard_phases.qmlpart
---

## Decided
Every probe step engine runs a step only after the item tree (visibility, geometry, focus item, tab-stop flags) held still for one tick; stall budget is about 60 s of wall clock. Any watchdog around a probe must be longer than that budget (rules probe 80 s, runner 150 s), or the watchdog kills a probe the budget would have let finish.

## Tried and ruled out
Only raising the 200-tick budget: not enough, the Tab-order and wrong-focus failures come from acting before layout settles. The settle gate cut load failures from 48/165 to 16/165; it cannot catch wrong-focus or extra-key symptoms (answer_surfaces, connections, blocklists), which still need per-case gates on the effect.

## Watch out
Drawer probe is not a step engine (one synchronous run, 45 s watchdog) and was not ported; its failures include `Cannot read property 'pageStack' of null`, possibly a real ordering bug. Do not run the test binaries from target/debug/deps under the real HOME without checking learned.json is untouched.

In the code: crates/snitchwatch-kirigami/tests/support/list_keys.qmlpart (treeSignature), tests/support/probe_runner.rs (150 s outer watchdog), tests/rules_keyboard_qml.rs (80 s). Reproduce with 3 x nproc `yes` loops, QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic.
