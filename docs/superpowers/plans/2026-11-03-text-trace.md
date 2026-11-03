# SNITCHWATCH_TEXT_TRACE: opt-in on-screen text change log

Status: implementing. GUI-only (`crates/snitchwatch-kirigami`); no bridge, wire,
daemon or image change. Main was at `aaf36f36`.

## Why

`docs/superpowers/research/2026-11-01-title-flicker-3.md`: three attempts with
synthetic probes failed to reproduce the owner's flicker (a one-line word such
as "working"/"waiting" flashing over the Daemon Health / Security Scan title
area while scrolling). The owner's Breeze style cannot be run here, so the
app itself must say which item flashes.

## Design

- `SNITCHWATCH_TEXT_TRACE=1` (exactly `1`, like `SNITCHWATCH_DEMO`) sets
  `appInfo.textTrace` (new AppInfo qproperty, read in Rust).
- `qml/TextTrace.qml`, self-contained. `main.qml` mounts it through
  `Loader { active: appInfo.textTrace }`, so with the flag off nothing is
  instantiated (no walker, no connections, no timers). A few lines in
  main.qml (guarded by the 800-line cap).
- Change-driven: samples on the window's `frameSwapped` (no timer). Each
  sample walks the window content and `Controls.Overlay.overlay` (popups,
  tooltips) and keeps, per text item (anything with string `text` and
  `lineCount`), effective visibility, effective opacity and text.
- Logs only diffs, one line each, via `console.log` (stderr, so it reaches the
  terminal/journal without `SNITCHWATCH_LOG`):
  `text-trace t=<ms> <path> <field>: <old> -> <new>`, plus `+`/`-` lines for
  items that appear/disappear. Path = per-level `Type#childIndex[objectName]`.
  Items hidden before and after are skipped (noise).
- The tray tooltip (`trayController.tooltip`) is passed in as a property and
  logged on change as `tray.tooltip`.
- Each line is also emitted as the `traced(line)` signal; the qmltestrunner
  test asserts on it.

## Tests (written first)

- `tests/qml/tst_text_trace.qml`: stub tree; change a Text, hide an item for a
  few ms, show a new one, change the tray tooltip; assert path/field/old->new.
  Also asserts a no-change sample logs nothing.
- `tests/text_trace_qml.rs` (source guard): main.qml mounts TextTrace only
  behind `appInfo.textTrace`, registered in build.rs, main.qml under cap.
- Rust unit test: only exactly `1` enables the flag.

## Not verified here

The owner's Breeze run. This only collects evidence.
