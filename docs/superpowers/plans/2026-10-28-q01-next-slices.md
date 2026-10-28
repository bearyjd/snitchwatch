# Q01, next slices: keyboard and screen reader beyond the Connections page

Status: **draft, awaiting owner approval** (owner request 2026-10-28: plan
the rest of the GUI). Docs only; no code. Main was at `a05c1b4` when this
was written.

Parent: roadmap row Q01
(`../specs/2026-10-24-competitive-feature-roadmap.md`). The first slice,
the Connections page, shipped in #242 (`2026-10-27-q01-connections-keyboard.md`).

All of this is GUI-only (`crates/snitchwatch-kirigami`). If a slice turns
out to need a bridge, wire, daemon or image change, it stops (see the stop
rules).

## What #242 established (reused by every slice)

- **Real key events only.** QtTest `keyClick` on the window. Never call a
  handler, never check source strings.
- **Readiness gates.** Each probe phase waits until what it touches exists,
  is shown and enabled, and its geometry has held for a tick. The cap is
  ~200 ticks, after which it fails naming what it waited for. This is the
  #237 pattern, used again in #245 and #246.
- **Runtime name guard** (`tests/connections_accessible_names_qml.rs`):
  - turn Qt's accessibility layer on, through the test-only helper
    `tests/support/accessibility.h` and `src/accessibility_test.rs`;
  - walk the live object tree, window content and popup overlay included;
  - fail any shown control with an interactive role and no name, a generic
    name, or (for a combo box) only its value;
  - assert exact names, and check again after a value changes.
- **Mutants.** Break each fix on purpose and watch a named test fail.

## Inventory (from the code at `a05c1b4`)

**How it was made.** A static scan of every file in `qml/` counted the
interactive controls in each surface. These are Button, ToolButton, Switch,
CheckBox, RadioButton, ComboBox, TextField, TextArea, SpinBox, MenuItem,
ItemDelegate and Kirigami.Action. For each control the scan noted whether
it sets `Accessible.name`, or only a `text:`. A Qt Quick Control's default
accessible name is its text, but for a field or a combo box the text is the
value, not a label. I then read the key handling of the prompt surfaces by
hand.

**Treat the counts as leads, not results.** The runtime guard is the real
check, and each slice starts by running it.

**Columns:**
- *Controls:* how many, of which kinds.
- *No name:* controls with neither `Accessible.name` nor `text:`.
- *Keyboard:* what is known.
- *Runtime probe:* tests that load the real QML.
- *Key events:* tests that send real key events to it.

### Prompt and answer surfaces (security-critical decisions)

| Surface | Controls | No name | Keyboard | Runtime probe | Key events |
|---|---|---|---|---|---|
| `AlertWindow.qml` (the standalone prompt) | 2 Buttons, 2 `SegmentedChoice` rows (scope, duration) | 0, but the two choice rows have no group name ("Applies to" and "For how long" are separate labels) | Return = Allow, Esc = Deny, after an arming delay and a held-key guard, all in `handleKey()`. The verdict buttons call `answer()` directly (`onClicked`), which checks arming but **not** held or repeated keys. The `FocusScope` (`keys`) has `Keys.onPressed` that accepts every key, so Tab is probably swallowed before it reaches the choices. **Unverified**: no Tab/Shift+Tab test exists; slice 1's first step records the real behaviour. | `alert_window_qml.rs` | none (it calls `handleKey` directly; a source guard pins `Keys.onPressed`) |
| `PendingDecisionSheet.qml` (inspector decision) | 2 ComboBoxes, 2 Buttons | 0 (named "Scope: …" / "Duration: …" in #242) | Reachable via the inspector; Tab order unchecked | `sheet_old_bridge_qml.rs`, `verdict_not_remembered_qml.rs`, #242 guard | #242 (focus only) |
| `MakeRuleSheet.qml` (+ `MakeRuleOutcomes.qml`) | 3 Buttons, 2 ComboBoxes | 2 (both combo boxes) | Overlay sheet; Tab order and Escape unchecked | none | none |
| `DecideLaterButton.qml` | 1 Button | 0 | Part of the row/inspector Tab order (#242 covers the row) | via #242 | via #242 |
| `PromptSlotBanner.qml` | 4 Kirigami.Actions | 0 | InlineMessage actions; reachability unchecked | `prompt_slot_banner_qml.rs`, `deferred_rows_qml.rs` | none |
| `DnsConsentDialog.qml` | 2 Buttons | 0 | Dialog; default button and Escape unchecked | `curated_groups_step_qml.rs:166-168` opens and checks the real consent dialog | none (key-event coverage is absent) |

### Rules

| Surface | Controls | No name | Keyboard | Runtime probe | Key events |
|---|---|---|---|---|---|
| `RulesPage.qml` | 7 Buttons, 1 ItemDelegate (rule rows) | 1 | Rows are delegates; arrow/Enter and menu by keyboard unchecked | 10 probes (hits, insights, count hint, …) | none |
| `RuleEditorSheet.qml` | 4 TextFields, 4 ComboBoxes, 3 Switches, 1 CheckBox, 6 Buttons (static declarations; the condition row repeats at runtime) | 8 unnamed non-text controls: 4 ComboBoxes, 3 Switches, 1 CheckBox. The 4 TextFields are value-bound (name = value) and count separately | Overlay sheet; unchecked | `rule_editor_sheet_qml.rs`, `profile_rules_qml.rs` | none |
| `RuleSimulatorSheet.qml` | 11 TextFields, 2 ComboBoxes, 2 TextAreas, 1 SpinBox, 1 CheckBox, 2 Buttons | 16 | Overlay sheet; unchecked | `simulate_connection_qml.rs`, `rules_simulator_advanced_qml.rs` | none |
| `RulesInspectorSheet.qml` | 5 Buttons, 1 Switch | 1 | unchecked | `rules_all_apps_qml.rs`, `rules_insights_qml.rs` | none |
| `RulesImportSheet.qml` | 2 Buttons, 1 CheckBox | 1 | unchecked | `rules_import_sheet_qml.rs` | none |
| `RuleButton.qml`, `LevelRuleActions.qml` | menu + confirm dialog | 0 | Rule button reachable in #242's row Tab order; the confirm dialog's keys unchecked | `rule_button_qml.rs`, `level_rule_*_qml.rs` | none |

### Learning review

| Surface | Controls | No name | Keyboard | Runtime probe | Key events |
|---|---|---|---|---|---|
| `LearningReviewPage.qml`, `LearningReviewRow.qml` | 8 Buttons, tick CheckBoxes | 0 | Labels toggle their box by tap (`TapHandler`); the box itself is focusable | `learning_review_qml.rs` | Escape and Return on the summary dialog only |
| `LearningSummaryDialog.qml` | 2 Buttons, extra-confirm CheckBoxes | 0 | Escape = discard and Return does nothing are tested; Tab to Apply is unchecked | same | yes (2 keys) |
| `LearningStartPanel.qml` | ComboBox, CheckBox, 2 Buttons | 1 (the length combo box) | unchecked | same | none |
| `LearningBanner.qml`, `LearningProgressList.qml` | 2 Actions, 1 Button | 0 | unchecked | `learning_banner_qml.rs`, `learning_end_qml.rs` | none |

### Other pages and surfaces

| Surface | Controls | No name | Keyboard | Runtime probe | Key events |
|---|---|---|---|---|---|
| `RecommendedRulesPage.qml` | 10 Buttons, 1 Switch, 1 ItemDelegate | 2 | unchecked | `recommended_rules_qml.rs`, `recommended_orphans_qml.rs` | none |
| `ProfilesPage.qml` | 10 Buttons, 3 TextFields, 1 ItemDelegate | 2 unnamed declarations: the new-profile-name TextField (placeholder only) and the row ItemDelegate (:176). The rename and network-matchers fields (:314) are value-bound | unchecked | `profile_rules_qml.rs`, `honest_ui_pages_qml.rs` | none |
| `BlocklistsPage.qml` | 6 Buttons, 1 TextField, 1 ItemDelegate | 2 unnamed declarations: the subscription URL TextField (:99, placeholder only) and the row ItemDelegate (:295) | unchecked | `blocklists_*_qml.rs` | none |
| `DaemonHealthPage.qml` + `DenyStatusSection.qml` | 2 Buttons | 0 | unchecked | `daemon_health_model_qml.rs`, `deny_status_qml.rs` | none |
| `DiagnosticsPage.qml` | 2 Switches, 2 ComboBoxes, 2 Buttons, 1 TextArea | 4 | unchecked | `honest_ui_external_text_qml.rs` | none |
| `ScannerPage.qml` | 1 Button | 0 | unchecked | `scanner_*_qml.rs` | none |
| `GeoPage.qml`, `GeoDatabaseSettings.qml` | 5 Buttons, 1 ItemDelegate | 0 | unchecked | `geo_qml.rs` | none |
| `TrafficPage.qml` | none interactive | n/a | n/a | `traffic_model_qml.rs` | n/a |
| Map on the Connections page: `MapBand.qml` (embedded by `MonitorPane.qml` ~52), `MapView.qml`, `MapMarker.qml`, `MapDestinationsPopup.qml` | band buttons (show/hide, zoom, fit, scope, destinations), markers, popup list with ItemDelegates | band buttons and markers are named (`MapBand.qml` 92-177, `MapMarker.qml:36`); the popup delegate sets no `Accessible.name` and carries text only through a `contentItem` Label | `MapView` handles +, -, Home/0 and arrows; markers have `activeFocusOnTab`; the popup takes Return/Enter and focuses its list on open. Escape and focus restoration unchecked | `map_view_qml.rs` (names and `activeFocusOnTab`), `map_drilldown_qml.rs`, `map_split_drag_real_qml.rs`, `honest_ui_map_guards.rs`, and the CI QML runner's `tests/qml/tst_map_camera.qml` and `tst_map_drilldown.qml` | `tst_map_camera.qml:172,211-222` (real Tab, Plus, Right, Home on production QML); `tst_map_drilldown.qml:192-200` (Return chooses a popup destination). Marker activation, Escape and focus restoration have none |
| Banners: `LearningBanner.qml` (Stop learning, Review, ~65-73), daemon-health banner Details action (`main.qml` ~435-450), `LearningProgressList.qml` Cancel (~39-42) | 3 Kirigami.Actions, 1 Button | 0 | InlineMessage actions; reachability unchecked | `learning_banner_qml.rs`, `learning_end_qml.rs`, `daemon_health_model_qml.rs` | none |
| `AnalysisExportSheet.qml` | 4 Buttons, 3 CheckBoxes, 3 TextFields, 1 ComboBox, 1 TextArea | 0 static (5 explicit names) | unchecked | `tests/probes/analysis_export_probe.qml` (~15-76) instantiates and operates the real sheet, plus `analysis_export_qml.rs` and a lifecycle probe | none |
| `OnboardingPage.qml`, `CuratedGroupsStep.qml`, `SilentModeChoice.qml` (first run) | 6 Buttons (3 + 2 + 1), 1 Switch (repeated per group at runtime), 1 RadioButton declaration, 1 Kirigami.Action (`SilentModeChoice.qml:70`, omitted from the earlier scan) | 1 | unchecked | `wizard_qml.rs`, `curated_groups_step_qml.rs` (opens and checks the real DNS consent dialog, ~164-168), `silent_mode_onboarding_qml.rs` | none |
| `main.qml` (global drawer, page switching) | 12 Kirigami.Actions | 0 | Opening the drawer and switching pages by keyboard unchecked; no `Shortcut`s | `page_stack_lifecycle_qml.rs` | none |
| `TrayMenu.qml` | 8 `Qt.labs.platform` MenuItems: 6 actionable, 2 separators (`separator: true`) | 0 unnamed actionable entries (all six have non-empty text bindings; the two textless declarations are separators) | A native menu drawn by the desktop shell. Native menu export and D-Bus events are partly headless-testable (`tests/native-tray/`, offscreen, ~28-37); Plasma Wayland input/grab acceptance is not | `tray_menu_qml.rs` (model), `tray_lifecycle_qml.rs`, `tests/native-tray/` (real `TrayMenu.qml`, six exported entries, D-Bus) | none |

## Slices, in order (each its own small PR)

Each PR follows the overnight plan's steps:
1. a short plan note if non-trivial;
2. tests first, red, then the code;
3. the local gate (fmt, clippy `-D warnings`, kirigami crate tests, `cargo test`);
4. a PR;
5. `/codex`;
6. merge on green.

Each slice:
- adds a runtime name guard for its surfaces, and a real-key-event probe
  with readiness gates;
- fixes only what the guard and probe find, with no redesign;
- proves each fix with a mutant;
- lists what it does not cover.

### Slice 1: the standalone prompt (`AlertWindow.qml`)
- **Scope:** make the scope and duration choices reachable by keyboard, and
  give each choice row a group name, for example "Applies to: This host
  only".
  - Owner decisions D1 and D2 (below) are resolved: Tab works in the
    prompt; Return on a choice only selects it.
  - The arming delay and the held-key guard must not change in effect.
- **First step: a baseline probe.** Before any change, send real Tab and
  Shift+Tab to the armed window and record where focus goes. The claim that
  Tab never reaches the choices is unverified until this runs.
- **Safety boundary.** `answer()` checks arming only. Held-key and repeat
  protection lives in `handleKey()`, and the verdict buttons call `answer()`
  directly. Once children can take focus, native button dispatch (Space,
  Return, keypad Enter on a focused button) is a new path to a verdict that
  skips the held-key guard. The slice must close it, or prove it cannot
  matter.
- **Tests first** (real press, hold and release via QtTest, not
  `handleKey` calls; existing tests call `handleKey()` directly):
  - Pre-Tab path (proves D1's "Return before any Tab"): with the initial
    focus on the `FocusScope`, a real Return gives exactly one Allow and a
    real Escape exactly one Deny, asserting the payload (row, verdict,
    scope, duration). Also: before arming nothing is sent; a key held from
    before arming, an auto-repeat, and a release then a fresh press behave
    as in the held-key guard. Disabling the initial Return or weakening its
    held-key protection must fail one of these (mutants);
  - Space, Return, keypad Enter and Escape on each choice button AND on
    each verdict button, covering: a key held from before arming, an
    auto-repeat, and a release then a fresh press;
  - the same across row replacement (a new row arrives while a key is held)
    and duration re-arming (`onRemembersChanged`);
  - every case asserts the exact verdict, scope, duration and submission
    count; activating a choice submits nothing;
  - Tab and Shift+Tab visit scope, duration, Allow, Deny in order;
  - the name guard covers the window and asserts exact names.
- **Source-wiring guard.** `alert_window_qml.rs` (~331-338) pins that
  `Keys.onPressed` accepts every key and calls `handleKey`. Any change to
  key interception must update that guard together with the event tests, in
  the same commit.
- **Guard helper ownership.** This slice is already a second consumer of
  the name guard (Connections is the first). The shared helper
  `tests/support/a11y_guard.qmlpart` arrives with PR #253. Order: slice 1
  may merge with a local snippet, then both the Connections test and the
  Alert test migrate to the helper in slice 2, or the helper is extracted
  earlier.
- **Review focus:** no new way to send Allow by accident. Every path to a
  verdict goes through `answer()` and the arming check, and the held-key
  guard applies to native button activation too.
- **Does not cover:** whether the compositor actually focuses the alert
  window. Under Wayland `requestActivate` can be refused; that needs a
  desktop.

### Slice 2: answer surfaces in the main window
- **Scope:** `MakeRuleSheet` and `MakeRuleOutcomes`, the
  `PendingDecisionSheet` Tab order, the `PromptSlotBanner` actions,
  `DnsConsentDialog`, and the `LevelRuleActions` confirm dialog.
  - Name the two MakeRule combo boxes the way #242 named the decision ones.
  - Escape closes a sheet or dialog without acting, and focus returns to
    whatever opened it.
- **Also in this slice: the daemon-health banner.** Its Details action
  (`main.qml` ~435-450) is reached by Tab and fired by keyboard, and
  focus after navigation lands on the new page.
- **Tests first:**
  - open each surface from its real trigger;
  - Tab through it and fire with Space;
  - Escape closes it, nothing is sent, and focus returns;
  - the guard runs with each surface open.
- **Shared helper (migrate here).** Slice 1 is already the second consumer
  of the guard's tree walk and its role and name rules (see slice 1,
  ownership). The helper `tests/support/a11y_guard.qmlpart` arrives with
  PR #253; this slice migrates both `connections_accessible_names_qml.rs`
  and the Alert test onto it, pulled in with `include_str!`/`concat!`. Move the readiness `stable()` helper there
  too. #242's guard then uses the shared copy, with no behaviour change: its
  mutants must still be killed.
- **Review focus:** the extraction loses no check; Escape never answers.
- **Does not cover:** the Connections-page modal sheets, beyond Escape and
  focus return.

### Slice 2b: the map on the Connections page
The map was excluded from the earlier Q01 plan and from #242; it is
embedded in `MonitorPane.qml` (~52), so this slice owns it. `GeoPage` is a
separate surface and stays in slice 6.
- **Scope:** `MapBand` controls, `MapMarker`, and
  `MapDestinationsPopup.qml` (~39-45), whose delegate has no text or
  `Accessible.name` of its own.
  - Name each popup delegate after its destination and summary.
  - Keyboard selection of a marker and of a popup entry; Escape closes the
    popup; focus returns to the control that opened it.
- **Tests first:** reuse the existing key-event tests that load production
  QML in the CI QML runner: `tests/qml/tst_map_camera.qml` (~172, 211-222:
  Tab, Plus, Right, Home) and `tst_map_drilldown.qml` (~192-200: Return
  chooses a popup destination). `map_view_qml.rs` (~131-136) checks names
  and `activeFocusOnTab`. Add only what is missing: marker activation by
  Space/Return, Escape closing the popup, and focus restored to the
  opener. The guard runs with the popup open.
- **Does not cover:** pan and zoom feel, or screen-reader behaviour on a
  canvas-like map.

### Slice 3: Rules page and its sheets
- **Scope:** `RulesPage` (rows, menus), `RuleEditorSheet`,
  `RuleSimulatorSheet`, `RulesInspectorSheet`, `RulesImportSheet`.
  - The editor and simulator have the most unnamed controls (the editor's
    8 are 4 ComboBoxes, 3 Switches and 1 CheckBox; its 4 TextFields are
    value-bound; the simulator's count comes from the runtime guard) that
    are labelled only by a neighbouring Label. Name each field after its label.
  - Combo boxes use the "Label: value" pattern, as in #242.
- **Tests first:**
  - keyboard: rows by arrows, the row menu by the Menu key, the editor by
    Enter, Tab through every field in visual order;
  - saving and cancelling by keyboard;
  - the guard runs with every sheet open and after a value change.
- **Review focus:** the Tab order matches the visual order; no field loses
  its existing name; the honest-UI guards stay green.
- **Existing work.** `origin/feat/q01-rules-keyboard` (commit `dd065b0`,
  unreviewed, forked from an older main) already holds work for this
  slice. Rebase it onto main after #253 merges rather than restarting.
- **Does not cover:** rule import from a file dialog, which is a native
  dialog.

### Slice 4: learning review
- **Scope:** the page, its rows, the summary dialog and the start panel,
  plus the learning banner (`LearningBanner.qml`: Stop learning and
  Review) and `LearningProgressList`'s Cancel.
  - Name the length combo box.
  - Banner actions are reached by Tab and fired by Space/Return, tested
    with deterministic fixtures. Elapsed-time behaviour stays excluded.
  - Tab reaches Apply only after every extra confirmation, and Return still
    never applies (already tested; keep it).
- **Tests first:**
  - Tab order through the rows' boxes;
  - Space ticks a box;
  - Tab to Review, then to Apply;
  - the guard with the dialog open.
- **Review focus:** Apply is still reachable only by its own button.
- **Does not cover:** the learning run itself (time-based).

### Slice 5: list pages
- **Scope:** `RecommendedRulesPage`, `ProfilesPage`, `BlocklistsPage`.
  These are ItemDelegate rows, toggles, and add/edit fields.
- **Tests first:**
  - rows by arrows;
  - toggles by Space;
  - fields named after their labels;
  - the guard on each page.
- **Shared helper (if needed).** If the three pages share row keyboard
  handling, put one probe helper in `tests/support/`. Don't add a shared
  QML component unless two pages already duplicate the code.
- **Does not cover:** the blocklist download progress (network).

### Slice 6: status and settings pages
- **Scope:**
  - `DaemonHealthPage` with `DenyStatusSection`;
  - `DiagnosticsPage` (4 unnamed controls);
  - `ScannerPage`, `GeoPage` and `GeoDatabaseSettings`, `TrafficPage`;
  - `AnalysisExportSheet`;
  - the first-run pages (`OnboardingPage`, `CuratedGroupsStep`,
    `SilentModeChoice`).
- **Tests first:** the guard on each page and keyboard reachability of every
  control. These are mostly single buttons, so one probe can visit several
  pages.
- **Does not cover:** the scanner's privileged run (polkit, needs the host).

### Slice 7: global navigation
- **Scope:**
  - `main.qml`: opening the global drawer and switching pages by keyboard
    alone, with focus landing on the new page's first control;
  - optional page shortcuts, see decision point D3.
- **Tests first:**
  - from a fresh window, keys alone reach every page;
  - focus is never left on a destroyed page (`page_stack_lifecycle`).
- **Does not cover:** Plasma keyboard and grab behaviour of the tray menu, which needs a desktop.

## Needs a real desktop (not verifiable headless)

- **What Orca actually speaks.** The guard checks names at Qt's
  accessibility layer, not at AT-SPI or in speech.
- **Focus rings.** Whether the focused control is visibly marked in the
  Breeze style. Headless runs use the Basic style.
- **High contrast.** Whether text and the non-colour verdict markers stay
  readable in a high-contrast colour scheme.
- **Plasma keyboard and grab behaviour.** Real Plasma Wayland input
  and compositor-grab acceptance of the tray menu. Export and D-Bus events of
  the native menu are headless-testable (`tests/native-tray/`, offscreen),
  and are already tested.
- **The standalone prompt's activation.** Whether KWin gives the alert
  window keyboard focus when it appears.

**The owner's 10-minute check on the Plasma laptop**, once per slice batch:

1. **Turn Orca on.** Use System Settings, Accessibility, Screen Reader, or
   run `orca &` in a terminal.
2. **Connections page.** Press Tab until the list is focused, then:
   - use arrows to move between rows; Orca should say each row;
   - press the Menu key; Orca should say each action as you arrow through;
   - press Escape.
3. **Standalone prompt.** Trigger one with a program that has no rule yet,
   for example `curl http://example.com`, while prompting is on.
   - Without the mouse, check that it has focus.
   - Tab to the scope and duration choices (after slice 1).
   - Press Escape to deny.
4. **Rules page.** Open the editor with Enter and Tab through it. Orca
   should name every field.
5. **Focus rings and contrast.** Watch that the focused control is always
   visibly marked. Then switch to a high-contrast colour scheme in System
   Settings, Colours, and look over Connections and Rules.
6. **Tray menu.** Open it with the keyboard. Plasma lets you give the system
   tray a keyboard shortcut in its settings; set one if none is set. Orca
   should say the items.

Write down anything Orca says wrongly or skips. Each problem becomes a
follow-up issue.

## Decision points (D1 and D2 resolved; the owner is asked about the rest before the slice starts)

- **D1, slice 1. Resolved by the owner 2026-10-28.** With focus on a
  scope or duration choice, Return ONLY selects that choice and never sends
  a verdict. Allow needs focus on Allow, or Return before any Tab. Escape =
  Deny always.
- **D2, slice 1. Resolved by the owner 2026-10-28.** Tab works in the
  prompt: scope, duration, Allow, Deny.
- **D3, slice 7.** Should there be page shortcuts (for example Ctrl+1…6),
  or only the drawer?
- **D4, all slices.** Is a value-only name acceptable anywhere, for example
  a field whose label is visually adjacent? The plan's default is no: every
  field gets "Label: value".
- **D5.** Is high contrast in scope for Q01, or for Q02 (scalable text and
  non-colour verdicts)? The plan assumes Q02.

## Stop rules (as in the overnight plan)

- **Too many review rounds.** After 4 `/codex` rounds still finding HIGH or
  CRITICAL, the PR is parked as a draft with a summary. No merge.
- **Out-of-scope change.** A slice that needs a bridge, wire, daemon or
  image change stops and is parked.
- **Repeated CI failures.** If CI fails twice on the same unrelated test,
  record it rather than retrying forever. A fixed-timer probe gets the
  readiness gate in its own PR (#245, #246).
- **Never without the owner:**
  - staging, rebooting or editing host files;
  - changing tower refs;
  - posting upstream;
  - force-pushing, except a lease-guarded rebase;
  - deleting worktrees or `dist/`.
- **Resource limits.** At most 3 agents at once, scratch output kept small,
  and `/tmp` checked before each build. If the quota fills, pause and
  report.
