# Selectable text: copy anything out of the application

Status: **PR A (plan + infrastructure + two pilots)**; later batches wait for
owner review of this doc. Owner request 2026-10-30: "make all the text
selectable for the next flatpak so I can copy it out of the application and
paste it."

GUI-only (`crates/snitchwatch-kirigami`). No bridge, wire, daemon or image
change. Main was at `878c6b4` when this was written.

## Owner decisions (resolved, not re-opened here)

1. Plain text becomes **mouse-selectable, read-only** text. Clickable list rows
   (anything where a click opens details or toggles) keep their click and gain
   a **right-click "Copy"** that puts the row's text on the clipboard.
2. **Mouse only.** Selectable text is not a Tab stop and must not change any
   tested Tab itinerary, focus order or screen-reader reading order. Keyboard
   users keep the existing Copy buttons. Where a row already uses the Menu key
   we do not change that (see "Flags").

## Inventory

Generated, not hand-counted: `crates/snitchwatch-kirigami/scripts/selectable_inventory.py`
(`--files` for the per-file split). It tracks the QML type stack by brace
depth and classifies each `Label`, `Text`, `Kirigami.Heading` and text field.
It is a heuristic (the first step of each batch re-checks its files by hand),
but it is deterministic and re-runnable. Output at `878c6b4`:

```
files with text items: 54   Label/Text/Heading/field items: 356
  class A: 221   plain static text                       -> convert
  class B:  30   elided / truncated                      -> decide per case
  class C:  47   inside a clickable row/delegate         -> right-click Copy of the row
  class D:  31   inside a button / action / menu / tooltip -> not converted
  class F:  27   existing text fields (TextField/TextArea/SearchField) -> leave
  class E:  38   Kirigami-owned InlineMessage / PlaceholderMessage      -> see below
  by type: Label=311, Heading=18, TextField=22, TextArea=4, SearchField=1
```

(A+B+C+D = 329 Label/Heading items; F are fields; E counts components, not
items, so it is not part of the 356.)

| Class | Decision |
|---|---|
| A plain static text | Replace with `SelectableText` (batches below). |
| B elided | `TextEdit` has no elide. Per case: (1) **wrap** if the layout can grow a line (names, paths in sheets: `wrapMode: Text.WrapAnywhere`); (2) else **keep the `Label`** with `ElideRight` and give its container `RowCopyMenu` (right-click Copy copies the *full* text, which is the real gain, since the elided part is what the user cannot read). B inside a clickable row is C. |
| C in clickable rows | Labels in the row stay `Label`. The row gets one `RowCopyMenu` with a plain-text string the caller builds (the same text the row announces: `Accessible.name` + description, or the Copy-details string where one exists). Drag-selecting inside an `ItemDelegate` would fight the click and the list's Flickable. |
| D button/action/menu/tab/tooltip/placeholder | Not converted, not selectable by design (controls are operated, not read). Includes `placeholderText`, `ToolTip.text`, `Kirigami.Action.text`, menu items, tab and drawer entry text. |
| E Kirigami components | `Kirigami.InlineMessage` and `PlaceholderMessage` draw their text inside Kirigami's own label, which has no selectable hook and which the honest-UI guards require us to leave as is (`honest_ui_external_text_guards.rs`: data goes in a PlainText label *beside* them). **Decision: leave them**; where one carries data worth copying, the data already sits in a `SelectableText` beside it. Page titles (`PageRow` header), `OverlaySheet` titles (our `SizedOverlaySheet` header, already a PlainText Heading: convert that one in the sheets batch) and the global drawer's action entries are D/E, not converted. List the leftovers in the final batch's PR. |
| F existing fields | Already selectable/copyable (`TextField`, read-only `TextArea` crash log, export path). Leave; the new Copy menu is not added. |

## Reusable pieces (this PR)

Kirigami in the target runtime (KF6, `org.kde.Platform` 6.11) and on this dev
box has **no `Kirigami.SelectableLabel`** (checked
`/usr/lib64/qt6/qml/org/kde/kirigami/qmldir`: no such type), so we build on
`TextEdit`:

- `qml/SelectableText.qml`: read-only `TextEdit`, `selectByMouse`,
  `persistentSelection`, `activeFocusOnTab: false`, `Accessible.role:
  StaticText`, theme colours, arrow cursor, `textFormat: PlainText` by
  default, `wrapMode: WordWrap` by default. API like `Label`: `text`,
  `color`, `font`, `wrapMode`, `horizontalAlignment`, `textFormat`. **No
  `elide`.** Right-click: lazily created menu with "Copy" (enabled with a
  selection) and "Select all".
- `qml/RowCopyMenu.qml`: a right-button `TapHandler` for a row; `copyText`
  is the plain text it puts on the clipboard through the existing
  `ClipboardCopy` (`NativeClipboard`, the mechanism of the About page's "Copy
  version info" button). Its "Copy" item ignores a Space auto-repeat like
  `GuardedButton.qml`.
- Files are added to `build.rs` and `tests/qml_guard_support/sources.rs`
  (append-only: other branches edit those lists).

## Pilots (this PR)

- **Class A**: `AboutPage.qml` (four `Label`s: row labels and values).
- **Class C**: the `ProfilesPage.qml` row delegate (one `RowCopyMenu`; the
  page's other text is left for batch 4).

Tests first: `tests/selectable_text_qml.rs` (mouse drag selects, copy lands on
the native clipboard, not focusable, role StaticText, right-click Copy puts the
expected plain text on the clipboard). Existing About and Profiles keyboard
tests run unchanged. Each guard is mutant-checked (evidence in the PR).

## Rollout batches (each one PR; no file appears in two batches)

Counts are class A / B / C from the script.

| # | Batch | Files | Notes |
|---|---|---|---|
| 1 | Status pages | `DaemonHealthPage`, `DiagnosticsPage`, `ScannerPage`, `DenyStatusSection`, `GeoDatabaseSettings`, `TrafficPage`, `MapView` | mostly A; Diagnostics has a read-only TextArea (F) |
| 2 | Onboarding and silent mode | `OnboardingPage`, `SilentModeChoice`, `LearningStartPanel`, `LearningBanner`, `PromptSlotBanner`, `BackgroundGroupBar`, `GroupPendingBadge`, `GroupVerdictCounts` | banners are Kirigami-owned (E): data beside them only |
| 3 | Learning review | `LearningReviewPage`, `LearningReviewRow` (C), `LearningProgressList`, `LearningLimitsList`, `CuratedGroupsStep` (C=5) | rows get `RowCopyMenu` |
| 4 | Profiles and recommended | `ProfilesPage` (rest), `RecommendedRulesPage` (C=9, B=2) | after PR A (shares `ProfilesPage`) |
| 5 | Blocklists and geo | `BlocklistsPage` (A=16, C=3, B=3), `GeoPage` (C=5) | |
| 6 | Rules page | `RulesPage` (C=7), `RulesInspectorSheet`, `RuleEditorSheet`, `RuleSimulatorSheet`, `RulesImportSheet` | Menu key flag below; big sheets, one PR |
| 7 | Prompt and sheets | `PendingDecisionSheet`, `MakeRuleSheet`, `LevelRuleActions`, `AlertWindow`, `SizedOverlaySheet`, `InspectorPanel`, `ConnectionExplanation` | the alert window is the security surface: PlainText stays |
| 8 | Connections and map | `ConnectionRow` (C), `ConnectionList`, `ConnectionsHeader`, `ConnectionsFilterBar`, `ProgramLabel`, `MapBand`, `MapMarker` | Menu key flag below; row text is built by `ConnectionDetailsCopy` |

`main.qml` (752 of 800 lines) is **not touched** by any batch; its one `Label`
stays a `Label`.

## Risks and how each is held

- **Accessible role.** A bare `TextEdit` reports `EditableText`. We set
  `Accessible.role: Accessible.StaticText` and `Accessible.readOnly: true`;
  the shared name guard already skips read-only editable text, and
  `StaticText` is not in its interactive list, so no name is demanded.
  The pilot test asserts the role at runtime. **Orca reading it twice** (item
  text and the label) cannot be tested headless: real-desktop check.
- **Focus.** `activeFocusOnTab: false`, `focus: false`, no focus ring (a
  `TextEdit` draws none). Decision: `activeFocusOnPress: true`, so a click
  gives focus and Ctrl+C works; the cost is that arrow keys then move a caret
  in that text instead of scrolling the page. If the owner dislikes that, flip
  to `false`: the right-click Copy still works because the selection is
  persistent. A Tab press from a clicked text moves to the next *existing*
  stop, so the itineraries (which start from a known item) do not change; the
  existing page tests run unchanged as the proof.
- **Layout equivalence.** `TextEdit` has no default `elide`, a different
  implicit size rule (no `implicitWidth` clipping), no `Label` palette/`font`
  inheritance from Controls (we bind `font` and `color` to the Controls
  palette), and a different default `textFormat` (we default PlainText, which
  is what the honest-UI rule demands anyway). `horizontalAlignment` and
  `wrapMode` use `TextEdit` enums; the wrapper keeps the `Text.*` names
  working (enum values are equal). Links: `onLinkActivated` is not wired
  (no class A text contains links; PlainText has none).
- **Drag conflicts.** `TextEdit` with `selectByMouse` inside a `Flickable` or
  `ListView` eats the press; the wheel and touch-drag still reach the
  Flickable only off the text. Hence rows (C) are never converted, only
  given a right-click menu, and class A text inside a `ListView` header is
  checked per batch.
- **Honest-UI source guards** (`tests/honest_ui_*.rs`). They scan for
  `Controls.Label {` / `Kirigami.Heading {` blocks that show external text
  without `textFormat: Text.PlainText`, and several assert `>= N` such
  labels exist. Converting a label to `SelectableText {` silently removes it
  from both checks. This PR extends the scan to `SelectableText {` blocks
  (the component defaults to PlainText and a guard forbids overriding it),
  and each batch lowers the affected `min_checked` by exactly the labels it
  converted, in the same commit, never more.
- **Name guard** may treat `TextEdit` as a control needing a name: held by
  the StaticText role (above) and proven by the unchanged About and Profiles
  name tests.
- **Menu pop-ups** are created lazily on the first right-click (no 300 menus
  at page load) and the row menu is a handler, not an item, so it adds no
  Tab stop and no reading-order node.
- **Primary selection / Wayland / X11** paste behaviour: real desktop only.

## Flags for the owner

- **Rules page** (`RulesPage.qml:480`): the Menu key already opens the
  inspector. Per decision 2 it is **not changed**; its right-click Copy
  therefore has no keyboard twin there (the inspector has the Copy buttons).
- **Connections list** (`ConnectionList.qml:99`): the Menu key / Shift+F10
  opens the row's actions. Same: unchanged; right-click Copy is added beside
  it in batch 8.
- Arrow keys after clicking selectable text move a caret (see Focus).

## Verification of this PR

`timeout 1800 just check`, `timeout 2400 cargo test -p snitchwatch-kirigami
--no-fail-fast` with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
What only a real desktop can show: the feel of drag selection, X11/Wayland
primary-selection paste, Orca not reading selectable text twice.
