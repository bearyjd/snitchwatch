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
}
