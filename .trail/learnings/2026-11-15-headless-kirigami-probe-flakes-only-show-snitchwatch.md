---
title: "Headless Kirigami probe flakes only show up under CPU load, so reproduce them with busy loops"
author: Snitchwatch
date: 2026-11-15
branch: fix/363-probe-save-race
cost: { minutes: 14, tokens: 13088 }
id: l-c1b0bcfe
scope: repo
status: active
src: agent
touches:
  - crates/snitchwatch-kirigami/tests/probes/status_pages_probe.qml
---

## Decided
Probe steps must gate on the effect they assert (the saved value), not only on the signal that triggers it. Reproduce QML probe flakes by running the test binary in a loop while 3x nproc `yes >/dev/null` loops run.

## Tried and ruled out
Looping the test with no load (25/25 passed) and with 2 busy loops (0/25): neither reproduced anything. 66 busy loops gave 2/20 failures.

## Watch out
The Tab-order flake in the geography rows and the recommended-page flake (probe_runner.rs:85) did not reproduce, so #363 is only partly fixed by PR #365.

In the code: crates/snitchwatch-kirigami/tests/probes/status_pages_probe.qml (the "Continue" step gate); run with QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic.
