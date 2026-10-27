# Q01, first slice: the Connections page by keyboard and screen reader

Status: build plan. Parent: `2026-10-27-overnight-autonomous-run.md` item 7;
roadmap row Q01 (`../specs/2026-10-24-competitive-feature-roadmap.md`).
GUI-only (`crates/snitchwatch-kirigami`): no wire, bridge, daemon or image
change. Change only what keyboard and screen-reader use need; no redesign.

## Scope
- The row context menu (`ConnectionActions.qml`):
  - it opens from the selected row with the Menu key or Shift+F10, also
    when a button inside that row has focus, because keys bubble up to the
    list (`ConnectionList.qml`);
  - arrows reach every action, Enter fires one, Escape closes the menu;
  - closing the menu puts focus back on whatever had it before. Qt's Popup
    already does this; the probe pins it, so no code was needed.
- The bulk actions: from a selected row, Tab goes to that row's own buttons
  (a header's Allow all / Deny all, a pending row's Allow) instead of the
  first row's. From there Tab and Shift+Tab move through them, and Space
  fires one (`ConnectionRow.firstTabStop`).
- Accessible names: every interactive control on the page has a meaningful
  `Accessible.name`. The only controls missing one were the pending
  decision's two combo boxes, which had their value but no label; they now
  say "Scope: …" and "Duration: …".

## Tests
- `tests/connections_keyboard_qml.rs` drives everything with real key events
  (QtTest `keyClick` on the window): Tab to the list, arrows, Menu and
  Shift+F10, every menu action reached by arrows, Enter, Escape and focus
  return, and the bulk bar's Tab, Shift+Tab and Space.
- `tests/connections_accessible_names_qml.rs` walks the real object tree at
  runtime, with Qt's accessibility layer turned on as an assistive
  technology would turn it (`tests/support/accessibility.h`; Controls fill in
  their role and name only then). It covers the page grouped and expanded,
  pending rows, the inspector, the exception-first bar, the title row, and
  both context menus.
  - Any shown control with an interactive role and no name fails.
  - Read-only text and a control's own parts (a combo box's display field)
    are not counted as controls.
- Both use the readiness-gate pattern from `connections_row_click_qml.rs`.

## Not covered
- Other pages (Rules, Blocklists, Profiles, Settings, Learning, Map page).
- Real screen-reader output (Orca), which needs a desktop session. The guard
  checks names at Qt's accessibility layer, not what Orca speaks.
