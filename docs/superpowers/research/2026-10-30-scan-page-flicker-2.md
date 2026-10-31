# Scan / Daemon Health title-area flicker, second attempt (2026-10-30)

Status: NOT reproduced. No production code changed.

Owner report (verbatim): "i have seen the flicker again. it is only when i
scroll over the title area. it looks like the word 'working' and 'waiting' are
in the flicker message. its one line and it also appears on the daemon health
page as well as the deep scan". Build: main 878c6b4.

## What was tried

A throwaway headless probe (not committed) loaded the REAL `qml/main.qml`,
navigated through the drawer actions to Security Scan and Daemon Health, and
for each page sent 30 mouse-wheel events (alternating direction, 48 ms apart,
then 1.5 s settle) with the pointer at y=20 (title), y=45 (toolbar), y=90
(header) and y=300 (body). Every 16 ms it walked the item tree of the window
content item and of `Overlay.overlay` and logged any change of text, effective
visibility or opacity of every Text/Label/TextEdit/InlineMessage text item,
plus any newly created non-empty one (tooltips live in the overlay).

Result: ZERO changes, on Basic, Fusion and Material styles, with the bridge in
its default no-daemon state (bridge banner and daemon-health model in their
startup state). `org.kde.desktop` (Breeze) is NOT installed in this sandbox
(`qqc2-desktop-style` missing), so the owner's actual style could not be
tried. Bridge-connected-with-daemon was not exercised.

Side observation: at the 900x640 window both pages' content (354 and 421 px)
is shorter than the viewport (594 px), so the pages are not scrollable at all
and wheel events there cannot move content.

## Ruled out (by source reading and the probe)

- Nothing in `ScannerPage.qml`, `DaemonHealthPage.qml`, `main.qml` or
  `GlobalDrawer.qml` has a HoverHandler/WheelHandler/MouseArea on the header or
  a binding on `contentY`/`moving`/`flicking`. `ScannerPage` resets
  `flickable.contentY = 0` only on `summaryText`/`busy` change.
- `HoverTip.qml` tooltips are hover-bound but only on list rows.
- The bridge banner, daemon-health banner, pending-exposure banner, prompt-slot
  banner and learning banner are all state-bound, not input-bound.

## Strings containing wait/work that can be on screen or in a tooltip

- `daemon_health_model.rs:49` `SUMMARY_PENDING` = "Waiting for complete daemon
  health information": the initial `statusSummary`, shown in the
  `daemonHealthStatus` label on Daemon Health until the three checks
  (DaemonReachable, FirewallRunning, NftablesSupport) are each exactly one `Ok`.
  It flips to "Daemon reachable; firewall checks passed" or to the Problem
  sentence (which also drives the `daemonHealthBanner` over every page).
  A one-line "Waiting ..." that flips on and off is plausible here if the bridge
  re-sends diagnostics reports that alternate between a complete and an
  incomplete check set. This is the best lead and is independent of scrolling.
- `tray.rs:29` / `tray.rs:401` "Snitchwatch — waiting for the bridge" (tray
  tooltip, startup only).
- `OnboardingPage.qml:95` "Waiting for the bridge"; `TrafficPage.qml:84`
  "Waiting for daemon statistics"; `RulesEmptyPlaceholder.qml:16`,
  `ConnectionActions.qml:238`, `curated_defaults.rs:292` "Waiting for the
  firewall (service's rules)".
- No user-visible string contains "working" as a status; the nearest are
  "Scanning…" (ScannerPage banner and button) and "Checking…"
  (`settings_controller.rs:240`, Settings page).

## What I need from the owner (one thing)

Either a 5-second screen recording of the scroll with the flicker visible, or:

    QT_LOGGING_RULES='qt.qml*=true' flatpak run --user --branch=r15.1-candidate org.snitchwatch.Snitchwatch 2>&1 | head -200

captured while scrolling over the title area. Also useful: whether the window
title bar (the compositor's, above the app) or the app's own header is meant by
"title area", since a compositor titlebar wheel action (KWin) is outside the app.
