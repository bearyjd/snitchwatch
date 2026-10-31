# Rounded, iOS / Little Snitch 6 style pass (PR A: infrastructure + pilot)

Owner request 2026-10-30: "we also need to still improve the clickability. make
the gui more rounded and more like an iphone / ios. use Little Snitch's GUI as
inspiration. layout is good."

Scope: styling and sizing only. Page structure, ordering, content, accessible
names/roles and the Tab itineraries are unchanged.

## 1. Audit: how controls are drawn today

### Which QQC2 style does the app really run?

- `src/main.rs` sets `QT_QUICK_CONTROLS_STYLE=Basic` **only** when
  `QT_QPA_PLATFORM=offscreen` and no style is set. Nothing else in `src/`,
  `justfile` (screenshots and `qml-test` pass `Basic` explicitly), the Flatpak
  manifests (`packaging/flatpak/*.yml`: no `QT_*` env) or `packaging/` sets a
  style.
- The Flatpak runtime is `org.kde.Platform//6.11`, which ships
  `org.kde.desktop` (qqc2-desktop-style, drawn through Breeze/QStyle).
  Kirigami selects it on a Plasma session.
- **Finding: on the owner's Plasma desktop the app runs the native
  `org.kde.desktop` (Breeze) style. Every test and every screenshot runs
  `Basic`.** So tests prove behaviour and geometry, never the Breeze look.
  Whether Kirigami really resolves to `org.kde.desktop` inside the Flatpak is
  inferred from the above, not observed here; the owner can confirm with
  `QT_LOGGING_RULES="qt.quick.controls.*=true"` or by looking at the focus
  frame (Breeze draws a glow, Basic a 2 px border).
- Consequence: any control that is not given its own `background:` /
  `indicator:` / `contentItem:` is drawn by Breeze (rounded ~3-5 px, ~30 px
  tall, native focus frame). Overriding those three parts in a wrapper replaces
  Breeze's drawing for that control, **including its focus frame**, so the
  wrapper must draw its own rounded focus ring (from `visualFocus`).
  Accessibility is unaffected: role, name, focus and `Keys` come from the
  QQC2 template (`T.Button`...), not from the style.

### Inventory (`crates/snitchwatch-kirigami/qml`, counted 2026-10-30)

| Control | Plain `Controls.*` uses | Via a `Guarded*` wrapper |
|---|---|---|
| Button | 72 | `GuardedButton` (56 uses across all four wrappers) |
| Switch | 10 | `GuardedSwitch` |
| RadioButton | 1 | `GuardedRadioButton` |
| ToolButton | 3 | `GuardedToolButton` |
| CheckBox | 9 | none |
| ComboBox | 14 | none |
| TextField / TextArea | 22 | none |
| ItemDelegate | 2 | none (rows are `NavigableRow` / delegates) |

Only two files use `background:` today (`BackgroundGroupBar`, `MapMarker`).
`Theme.qml` is a singleton with verdict colours, spacing and a type scale; no
radii or sizes. `Kirigami.InlineMessage`, `Kirigami.Separator`, overlay sheets
and the drawer are Kirigami-drawn.

**Main correction to the "style the Guarded wrappers centrally" idea:** the
wrappers cover ~45% of buttons and no ComboBox, CheckBox or text field, and the
two obvious pilot pages (About: one plain Button; Diagnostics: plain Buttons,
two ComboBoxes, a TextArea) are mostly plain controls. A central restyle of the
four wrappers alone cannot make a page uniformly rounded, and must not be
described as if it did.

## 2. Design tokens (appended to `Theme.qml`)

| Token | Value | Notes |
|---|---|---|
| `radiusSmall` / `radiusMedium` / `radiusLarge` | 6 / 10 / 14 | fields, small chips / buttons-as-rects, cards and sheets |
| `radiusPill` | 999 | Qt clamps to half the shorter side: capsule |
| `hitMin` | `max(44, gridUnit*2.5)` | minimum hit AND visible control height |
| `controlPaddingH` | `2 * largeSpacing` | pill buttons need horizontal air |
| `borderWidth` / `separatorWidth` | 1 / 1 | hairlines, alpha of text colour |
| `focusRingWidth` | 2 | drawn inside the bounds so a clipping parent never hides it |
| `hoverOpacity` / `pressedOpacity` | 0.08 / 0.16 | overlay of the text colour |
| `fillOpacity` | 0.08 | resting fill of a secondary control |
| `disabledOpacity` | 0.4 | |

Elevation: Little Snitch 6 is flat; separation is a hairline plus the fill, no
drop shadows (also cheaper on the software renderer). No elevation token.

Colours stay out of Theme: surfaces read the control's own `palette`
(`buttonText`, `highlight`, `highlightedText`, `windowText`, `base`), which is
the system palette in light and dark. No hex values.

## 3. Control by control

Legend: **W** central wrapper; **P** per-page swap needed; **S** Kirigami/native,
not restylable without replacing it.

| Control | Approach | Batch |
|---|---|---|
| Button | **W** `RoundedSurface` + `RoundedButton` (plain behaviour); `GuardedButton` becomes a `RoundedButton` (same Keys). Plain `Controls.Button` -> `RoundedButton` is **P** (72 sites) | PR A (wrappers + pilot), B-D (sites) |
| ToolButton | **W** `GuardedToolButton` (44x44 minimum, flat capsule). 3 plain sites **P** | PR A |
| Switch | **W** `GuardedSwitch`: iOS 51x31 track, round knob, 44 high. 10 plain sites **P** | PR A |
| RadioButton | **W** `GuardedRadioButton`: ring + dot, 44 high | PR A |
| CheckBox | not wrapped yet; new `RoundedCheckBox` + 9 **P** sites | batch C |
| ComboBox / selector | new `RoundedComboBox` (keeps the held-key guards now copy-pasted per site) + 14 **P** | batch C |
| TextField / TextArea | new `RoundedField` (radiusSmall, 44 high for single-line) + 22 **P**; owner decision on fields in dense lists | batch D |
| SegmentedChoice | one file, all its segments are plain Buttons: restyle in place as a joined capsule | batch B |
| ItemDelegate / list rows | row height >= 44 via `NavigableRow` and the two delegates; selection highlight rounded `radiusMedium` inset; **P** per list page, may conflict with the Label -> SelectableText branch | batch E |
| Cards, InlineMessage | `RoundedSurface` in card mode for hand-made cards; `Kirigami.InlineMessage` is **S** (leave) | batch E |
| Dialogs / sheets | `SizedOverlaySheet` / `Kirigami.Dialog` corner radius: sheets are Kirigami-drawn (**S**); only inner content changes | batch E |
| Drawer entries | `Kirigami.GlobalDrawer` actions are Kirigami-drawn (**S**): row height via `Kirigami.Units`, not shape | batch E |
| Badges / chips | `GroupPendingBadge` etc.: capsule radius, **P** | batch F |
| Scrollbars | **S** (Breeze draws them; thin by design). Not touched | - |
| Tooltips | `HoverTip` only: radiusSmall; native `ToolTip` is **S** | batch F |

## 4. Strategy decision (OWNER'S CALL)

- **(a) Shared surface + wrappers, swap sites per batch (recommended).**
  `RoundedSurface` draws fill, hairline, hover/pressed overlay and the rounded
  focus ring; wrappers use it. Incremental, reversible per batch, tests assert
  hit boxes. Cost: the app looks mixed while the rollout runs, and wrapped
  controls look the same on Basic and Breeze while unwrapped ones stay native.
- **(b) Switch the whole app to `Basic`** (set `QT_QUICK_CONTROLS_STYLE=Basic`
  in the manifests). Everything consistent with the tests and screenshots, but
  loses native Breeze rendering everywhere (scrollbars, menus, combo popups, text
  fields) and platform consistency; a11y roles are unaffected, but high-contrast
  and platform behaviour are only as good as Basic's.
- **(c) A project-local QQC2 style module.** One switch restyles everything,
  but a style is a large, separately-maintained component; disproportionate
  here.

Recommendation: (a). Revisit (b) only if the mixed look is rejected after
batch C.

## 5. Rollout (no two batches touch the same files)

- **PR A (this PR):** `Theme` tokens, `RoundedSurface`, `RoundedButton`, the
  four Guarded wrappers, probe + source guard, pilot pages **About** and
  **Silent-mode choice** (uses Guarded radio + buttons), screenshot runner
  entries.
- **B:** `SegmentedChoice`, alert window and `PendingDecisionSheet` buttons.
- **C:** `RoundedCheckBox`, `RoundedComboBox`; Diagnostics, Geo, Settings sites.
- **D:** `RoundedField`; Rules/Make-rule/Blocklists/Import sheets.
- **E:** list rows, cards, drawer entries (after the SelectableText branch
  lands to avoid Label conflicts).
- **F:** badges, chips, `HoverTip`, last polish.

Each batch: tests first (hit-box probe over the pages it touches), existing
keyboard tests unchanged, screenshots attached, `just check` + kirigami tests.

## 6. Risks

- Breeze vs Basic: geometry is verified; the look on Plasma is not (owner).
- Taller/wider controls can truncate text in tight `RowLayout`s: probe asserts
  `width >= implicitWidth`; narrow-window tests stay as the guard.
- Wrapper `GuardedButton` is documented for irreversible actions; plain
  buttons get `RoundedButton`, **not** `GuardedButton`, so no held-key
  semantics change silently.
- Wrappers keep the template root type, `Keys.*` handlers, `Accessible`
  properties and add only role-less, non-focusable visuals (no `MouseArea`,
  no extra Tab stops).
- `main.qml` is ~752 of 800 lines; PR A adds 3 lines.
- Colours from `palette`: dark mode is checked by reasoning and by the
  palette-only source guard, not visually.
- Owner decisions: strategy (a)/(b)/(c); fields in dense lists (batch D);
  whether switches take iOS proportions (51x31) or a plain 44 px row.
