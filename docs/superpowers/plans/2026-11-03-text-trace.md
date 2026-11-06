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
  sample walks the window content and `Controls.Overlay.overlay`.
- **Allowlist (owner decision, codex round 2):** text is read ONLY under an
  item that declares `property bool textTraceable: true`; the nearest
  declaration wins, so `textTraceable: false` opts a part of a marked subtree
  back out. Unmarked items are never read (no `text`, `displayText`,
  `selectedText`, `preeditText`), so editors, rule/connection tables, the Rule
  Simulator, inputs, process names and hosts cannot leak, however they are
  bound. The window title is read from Kirigami's `DefaultPageTitleDelegate`
  item (found in the page stack outside the current page; `globalToolBar` is
  a non-visual object and is not walked) only while the current page has `property bool textTraceTitle: true` (a literal,
  fixed title; ConnectionDetailPage's title is a process name and is not
  marked).
- Defence in depth on marked items: logged only while visible, effective
  opacity > 0 and FULLY inside the window and every `clip: true` ancestor
  (otherwise `(partly hidden)`); editable items (TextInput/TextEdit/TextField/
  TextArea, non-Normal `echoMode`, `readOnly: false`) log
  `<editable field redacted>`; truncated text logs `(elided)`.
- **Marked:** `main.qml` bridgeBanner (its `statusText` label opted out),
  daemonHealthBanner, pendingExposureBanner; the InlineMessage in
  `PromptSlotBanner.qml` (its holder line carries program and host, so it is
  not marked) and `LearningBanner.qml`; the InlineMessage in
  `DaemonHealthPage.qml`; the "Waiting for daemon statistics"
  PlaceholderMessage in `TrafficPage.qml`; the `ruleLabel` state label in
  `RuleButton.qml` ("Waiting for the firewall"); and `textTraceTitle` on the
  fixed-title pages (Traffic, Scanner, Daemon Health, About, Diagnostics, Geo,
  Silent Mode, Blocklists, Rules, Profiles, Recommended Rules, Learning
  Review, Connections, Curated Groups, Onboarding).
- **To add a mark:** one line `property bool textTraceable: true` on the item
  (only if every string it can show is fixed, none from the network, a file or
  the user), or `property bool textTraceTitle: true` on a page whose `title`
  is a literal. Do not add it in `main.qml` (800-line cap) unless it fits on an
  existing line.
- Logs only diffs, one line each, via `console.warn` (stderr, so it reaches the
  terminal/journal without `SNITCHWATCH_LOG`):
  `text-trace t=<ms> <path> <field>: <old> -> <new>`, plus `+`/`-` lines for
  items that appear/disappear. Path = per-level `Type#childIndex[objectName]`.
  Items hidden before and after are skipped (noise).
- The tray tooltip (`trayController.tooltip`) is passed in as a property and
  logged on change as `tray-tooltip (not necessarily visible): old -> new`.
  This is the one deliberate exception to the on-screen-only rule: the text is
  app-composed status ("waiting for the bridge" is a flicker candidate) and a
  tooltip is invisible to item-tree probing. The label keeps it from being
  mistaken for on-screen text. A tooltip starting `Blocked:` carries the
  attacker-influenced `RecentBlock.what` (src/tray.rs), and `compose_tooltip`
  appends more after it, so it cannot be split safely: it is logged only as
  `Blocked: (details withheld)`.
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
