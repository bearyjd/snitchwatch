// A `Kirigami.OverlaySheet` with concrete bounds.
//
// Kirigami 6.19 derives an OverlaySheet's implicitHeight from its `y` while
// deriving `y` from that same implicitHeight. Left alone, every sheet in this
// shell reports an implicitHeight binding loop and can grow past the window on
// smaller screens. Pinning width/height/y breaks the cycle.
//
// This lives in one place deliberately: the workaround was previously repeated
// verbatim at each sheet, so fixing one site silently left the others broken.
// When the upstream Kirigami cycle is fixed, delete the bindings here only.
//
// Usage — set `preferredWidth`; the inner layout binds the same value so the
// two can't drift if the overrides below are ever removed:
//
//     SizedOverlaySheet {
//         id: inspector
//         title: "…"
//         preferredWidth: Kirigami.Units.gridUnit * 22  // optional; 30 default
//         ColumnLayout {
//             Layout.preferredWidth: inspector.preferredWidth
//         }
//     }
//
// Property names deliberately avoid `contentWidth`, `topMargin`, `topInset`
// and friends: `Kirigami.OverlaySheet` derives from `Controls.Popup`, which
// already declares those, and redeclaring an inherited property makes the
// whole type fail to load.
//
// Accepted trade-offs of pinning these three bindings — all inherited from
// the per-sheet inline versions this component replaced, none introduced by
// consolidating them, and all pending a real-compositor check:
//
//   1. The inner layout's `Layout.preferredWidth` no longer decides width.
//      It feeds `implicitWidth`, which the `width` override below bypasses;
//      the content's actual width comes from the left/right anchors Kirigami
//      installs on it at runtime. The binding is kept as a correct hint (and
//      so removing the override restores stock behaviour), not because it is
//      load-bearing today.
//   2. Height is content-independent. Stock OverlaySheet derives it from
//      content + header + footer + padding; this pins every sheet to
//      `min(parent.height - topGap, maxSheetHeight)`, so a short sheet is as
//      tall as a long one.
//   3. `y` drops the stock `visualParent.y` term in favour of a flat
//      `topGap`, which can position a sheet differently on a page that has a
//      header or toolbar.
import QtQuick
import org.kde.kirigami as Kirigami

Kirigami.OverlaySheet {
    id: sheet

    // Preferred sheet width; it narrows below this on a small window but never
    // exceeds it. Also the size hint the inner layout should bind.
    property real preferredWidth: Kirigami.Units.gridUnit * 30
    // Upper bound on sheet height, minus `topGap` worth of breathing room.
    property real maxSheetHeight: Kirigami.Units.gridUnit * 32
    // Gap kept above the sheet so it never collides with the window chrome.
    property real topGap: Kirigami.Units.gridUnit * 3

    width: Math.min(parent ? parent.width : sheet.preferredWidth, sheet.preferredWidth)
    height: Math.min(parent ? parent.height - sheet.topGap : sheet.maxSheetHeight,
                     sheet.maxSheetHeight)
    y: parent ? Math.max(sheet.topGap, Math.round((parent.height - height) / 2)) : 0
}
