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
| `AlertWindow.qml` (the standalone prompt) | 2 Buttons, 2 `SegmentedChoice` rows (scope, duration) | 0, but the two choice rows have no group name ("Applies to" and "For how long" are separate labels) | Return = Allow, Esc = Deny, after an arming delay and a held-key guard. **Its `FocusScope` accepts every key press**, so Tab never reaches the scope or duration buttons: they are mouse-only. | `alert_window_qml.rs` | none |
| `PendingDecisionSheet.qml` (inspector decision) | 2 ComboBoxes, 2 Buttons | 0 (named "Scope: …" / "Duration: …" in #242) | Reachable via the inspector; Tab order unchecked | `sheet_old_bridge_qml.rs`, `verdict_not_remembered_qml.rs`, #242 guard | #242 (focus only) |
| `MakeRuleSheet.qml` (+ `MakeRuleOutcomes.qml`) | 3 Buttons, 2 ComboBoxes | 2 (both combo boxes) | Overlay sheet; Tab order and Escape unchecked | none | none |
| `DecideLaterButton.qml` | 1 Button | 0 | Part of the row/inspector Tab order (#242 covers the row) | via #242 | via #242 |
| `PromptSlotBanner.qml` | 4 Kirigami.Actions | 0 | InlineMessage actions; reachability unchecked | `prompt_slot_banner_qml.rs`, `deferred_rows_qml.rs` | none |
| `DnsConsentDialog.qml` | 2 Buttons | 0 | Dialog; default button and Escape unchecked | none | none |

### Rules

| Surface | Controls | No name | Keyboard | Runtime probe | Key events |
|---|---|---|---|---|---|
| `RulesPage.qml` | 7 Buttons, 1 ItemDelegate (rule rows) | 1 | Rows are delegates; arrow/Enter and menu by keyboard unchecked | 10 probes (hits, insights, count hint, …) | none |
| `RuleEditorSheet.qml` | 4 TextFields, 4 ComboBoxes, 3 Switches, 1 CheckBox, 6 Buttons | 8 (fields and combo boxes labelled only by a neighbouring Label) | Overlay sheet; unchecked | `rule_editor_sheet_qml.rs`, `profile_rules_qml.rs` | none |
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
| `ProfilesPage.qml` | 10 Buttons, 3 TextFields, 1 ItemDelegate | 1 (and the 3 fields are value-only) | unchecked | `profile_rules_qml.rs`, `honest_ui_pages_qml.rs` | none |
| `BlocklistsPage.qml` | 6 Buttons, 1 TextField, 1 ItemDelegate | 1 | unchecked | `blocklists_*_qml.rs` | none |
| `DaemonHealthPage.qml` + `DenyStatusSection.qml` | 2 Buttons | 0 | unchecked | `daemon_health_model_qml.rs`, `deny_status_qml.rs` | none |
| `DiagnosticsPage.qml` | 2 Switches, 2 ComboBoxes, 2 Buttons, 1 TextArea | 4 | unchecked | `honest_ui_external_text_qml.rs` | none |
| `ScannerPage.qml` | 1 Button | 0 | unchecked | `scanner_*_qml.rs` | none |
| `GeoPage.qml`, `GeoDatabaseSettings.qml` | 5 Buttons, 1 ItemDelegate | 0 | unchecked | `geo_qml.rs` | none |
| `TrafficPage.qml` | none interactive | n/a | n/a | `traffic_model_qml.rs` | n/a |
| `AnalysisExportSheet.qml` | 4 Buttons, 3 CheckBoxes, 3 TextFields, 1 ComboBox, 1 TextArea | 0 static (5 explicit names) | unchecked | none (the export probes drive the controller) | none |
| `OnboardingPage.qml`, `CuratedGroupsStep.qml`, `SilentModeChoice.qml` (first run) | 5 Buttons, 1 Switch, RadioButtons | 1 | unchecked | `wizard_qml.rs`, `curated_groups_step_qml.rs`, `silent_mode_onboarding_qml.rs` | none |
| `main.qml` (global drawer, page switching) | 12 Kirigami.Actions | 0 | Opening the drawer and switching pages by keyboard unchecked; no `Shortcut`s | `page_stack_lifecycle_qml.rs` | none |
| `TrayMenu.qml` | 8 `Qt.labs.platform` MenuItems | 2 static (texts are bound expressions) | A native menu drawn by the desktop shell: not testable headless | `tray_menu_qml.rs` (model only) | n/a |

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
  - Return/Escape semantics, the arming delay and the held-key guard must
    not change. Decision point D1 settles what Return does while a choice
    button has focus.
- **Tests first:**
  - Tab and Shift+Tab move through the choice buttons;
  - Space selects one;
  - Return and Escape still answer only after arming;
  - a key held since before arming still does nothing;
  - the guard covers the window, asserting exact names.
- **Review focus:** no new way to send Allow by accident. Every path to a
  verdict still goes through `answer()` and the arming check.
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
- **Tests first:**
  - open each surface from its real trigger;
  - Tab through it and fire with Space;
  - Escape closes it, nothing is sent, and focus returns;
  - the guard runs with each surface open.
- **Shared helper (extract here).** This is the second page to need the
  guard's tree walk and its role and name rules. Move them out of
  `connections_accessible_names_qml.rs` into one QML/JS snippet that each
  probe pulls in with `include_str!`/`concat!`, for example
  `tests/support/a11y_guard.js`. Move the readiness `stable()` helper there
  too. #242's guard then uses the shared copy, with no behaviour change: its
  mutants must still be killed.
- **Review focus:** the extraction loses no check; Escape never answers.
- **Does not cover:** the Connections-page modal sheets, beyond Escape and
  focus return.

### Slice 3: Rules page and its sheets
- **Scope:** `RulesPage` (rows, menus), `RuleEditorSheet`,
  `RuleSimulatorSheet`, `RulesInspectorSheet`, `RulesImportSheet`.
  - The editor and simulator have the most unnamed controls: 24 fields,
    combo boxes and switches that are labelled only by a neighbouring
    Label. Name each field after its label.
  - Combo boxes use the "Label: value" pattern, as in #242.
- **Tests first:**
  - keyboard: rows by arrows, the row menu by the Menu key, the editor by
    Enter, Tab through every field in visual order;
  - saving and cancelling by keyboard;
  - the guard runs with every sheet open and after a value change.
- **Review focus:** the Tab order matches the visual order; no field loses
  its existing name; the honest-UI guards stay green.
- **Does not cover:** rule import from a file dialog, which is a native
  dialog.

### Slice 4: learning review
- **Scope:** the page, its rows, the summary dialog and the start panel.
  - Name the length combo box.
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
- **Does not cover:** the tray menu, which needs a desktop.

## Needs a real desktop (not verifiable headless)

- **What Orca actually speaks.** The guard checks names at Qt's
  accessibility layer, not at AT-SPI or in speech.
- **Focus rings.** Whether the focused control is visibly marked in the
  Breeze style. Headless runs use the Basic style.
- **High contrast.** Whether text and the non-colour verdict markers stay
  readable in a high-contrast colour scheme.
- **The tray menu.** A native StatusNotifier/DBus menu drawn by Plasma.
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

## Decision points (the owner is asked before the slice starts)

- **D1, slice 1.** In the standalone prompt, while a scope or duration
  button has keyboard focus, should Return still mean Allow, or only select
  that choice?
  - Recommendation: Return selects the focused choice, and Allow needs
    Return with focus on Allow, or Return before any Tab.
  - Either way, Escape stays Deny.
- **D2, slice 1.** Should Tab be allowed in the prompt at all? Today a
  keyboard user can only accept the default scope and duration.
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
