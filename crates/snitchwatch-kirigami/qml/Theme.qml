// Shared look: verdict colours, spacing and type scale.
//
// Colours come from the active Kirigami palette (positive/negative/neutral/
// link/disabled text roles), so they stay readable on both light and dark
// colour schemes instead of being hardcoded hex values.
pragma Singleton
import QtQuick
import org.kde.kirigami as Kirigami

QtObject {
    // Verdict colours.
    readonly property color allow: Kirigami.Theme.positiveTextColor
    readonly property color deny: Kirigami.Theme.negativeTextColor
    readonly property color partial: Kirigami.Theme.disabledTextColor
    readonly property color needsRule: Kirigami.Theme.linkColor
    readonly property color pending: Kirigami.Theme.neutralTextColor

    // Spacing.
    readonly property int spacingSmall: Kirigami.Units.smallSpacing
    readonly property int spacingMedium: Kirigami.Units.largeSpacing
    readonly property int spacingLarge: Kirigami.Units.gridUnit
    readonly property int iconRow: Kirigami.Units.iconSizes.smallMedium

    // Type scale (multipliers of the platform default font size).
    readonly property real scaleSmall: 0.85
    readonly property real scaleBody: 1.0
    readonly property real scaleTitle: 1.25
    readonly property real scaleHeading: 1.6
    readonly property int basePixelSize: Kirigami.Theme.defaultFont.pixelSize > 0
        ? Kirigami.Theme.defaultFont.pixelSize : 14

    // Verdict token (ConnectionsModel `verdict` role) -> colour.
    function verdictColor(verdict) {
        switch (verdict) {
        case "pending": return pending;
        case "allowed": return allow;
        case "denied": return deny;
        case "blocklisted": return deny;
        default: return partial;
        }
    }

    // Rounded, iOS / Little Snitch 6 style tokens (docs/superpowers/plans/
    // 2026-10-30-rounded-ios-style.md). Radii, sizes and opacities only:
    // colours are read from each control's own palette, never fixed here.
    readonly property int radiusSmall: 6
    readonly property int radiusMedium: 10
    readonly property int radiusLarge: 14
    // Qt clamps a Rectangle radius to half its shorter side: a capsule.
    readonly property int radiusPill: 999
    // The least height (and, for icon-only buttons, width) of anything the
    // user can click or tap; the visible control is this size too.
    readonly property int hitMin: Math.max(44, Kirigami.Units.gridUnit * 2.5)
    readonly property int controlPaddingH: Kirigami.Units.largeSpacing * 2
    readonly property int borderWidth: 1
    readonly property int separatorWidth: 1
    readonly property int focusRingWidth: 2
    readonly property real fillOpacity: 0.08
    readonly property real borderOpacity: 0.18
    readonly property real hoverOpacity: 0.08
    readonly property real pressedOpacity: 0.16
    readonly property real disabledOpacity: 0.4
    // iOS switch proportions.
    readonly property int switchTrackWidth: 51
    readonly property int switchTrackHeight: 31
    readonly property int switchKnob: 27
}
