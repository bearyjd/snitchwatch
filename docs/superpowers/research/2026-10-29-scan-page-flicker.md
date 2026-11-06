# Security Scan page "flicker" before a scan (2026-10-28 recording)

Status: not a defect found; no code change. Owner input needed to close.

## Report
Before a scan has run, moving the mouse over the Security Scan page makes text
flicker. Recording: `Screencast_20261028_064848.webm` (27.7 s).

## What the frames show
Frames extracted with ffmpeg at 4.4-5.4 s and 13.8-21 s.

- 4.4 s: idle page, button reads "Run Deep Scan", showing its hover/focus
  outline with the pointer on it.
- 5.0 s: button reads "Scanning…" (disabled) and the BusyIndicator gear shows.
  The pointer has not moved.
- 5.4 s: a polkit "Authentication Required" dialog is open over the page.
- 13.8-20 s: still "Scanning…" + gear; the polkit dialog is up, with a failed
  password ("Authentication failure, please try again.") and retries.
- 21 s: the dialog is gone, the button is back to "Run Deep Scan" with the hover
  outline, and the report shows Informational (2) and Skipped (2).

So the "Scanning…" at about 5.0 s is a real scan started by a click (the polkit
prompt is the proof), not a hover artifact. "Scanning…" stays up for the whole
polkit prompt, by design: `busy` spans prompt plus scan. The hover effect is the
Basic-style button outline, which appears and disappears when the pointer enters
or leaves; it is not a text change.

## Code check (origin/main at ebfa0344)
`ScannerPage.qml` has no HoverHandler, MouseArea, ToolTip, Behavior or
animation on any text. Button `text`, `enabled`, the BusyIndicator `visible`
and the `scannerBusy` banner are all bound only to `controller.busy`, which
changes only in `ScannerController::run_scan` (set true on click, false when the
scanner thread's result is applied). Nothing on hover writes to the controller.
`Connections.onBusyChanged` only resets `contentY` and announces the outcome.

## Not done
No headless hover test was written. The frames already explain the visible
"Scanning…", and the code has no hover-bound state to count, so a test that
expects zero changes would pass by construction and prove nothing about the
reported flicker.

## What we need from the owner
- Which text flickers, and in which window state (before ever clicking the
  button, with no polkit dialog having been opened)?
- Hover target (button, the heading rows, the description label?), and whether
  it also happens on current main (post-#260/#263/#264), which the recording
  predates.
- Screen scale factor and whether the flicker is the button outline/colour
  (Basic style hover) rather than text content.
- A short recording of the flicker with no click in it.
