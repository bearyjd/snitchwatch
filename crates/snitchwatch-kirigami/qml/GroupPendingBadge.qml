// A group header's "N pending" badge: collapsed groups expose pending
// connections through it. Split out of ConnectionRow.qml for its line budget.
// The hairline capsule and a faint fill come from RoundedSurface (radiusPill);
// the label keeps the neutral text colour, so a filled badge cannot end up with
// the same colour for background and text (issue #31).
//
// The badge is NOT independently tappable: it sits on a clickable group-header
// row, which is the hit target (the row's hit area is at least Theme.hitMin).
// So its visible size stays small on purpose, it takes no focus, and its
// surface zeroes the 44 px implicit size a RoundedSurface otherwise has.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Control {
    id: badge

    property int count: 0

    focusPolicy: Qt.NoFocus
    leftPadding: Kirigami.Units.largeSpacing / 2
    rightPadding: Kirigami.Units.largeSpacing / 2
    topPadding: Kirigami.Units.smallSpacing / 2
    bottomPadding: Kirigami.Units.smallSpacing / 2
    Layout.alignment: Qt.AlignVCenter

    background: RoundedSurface {
        control: badge
        implicitWidth: 0
        implicitHeight: 0
    }

    contentItem: Controls.Label {
        id: pendingBadgeLabel
        textFormat: Text.PlainText
        text: badge.count + " pending"
        color: Kirigami.Theme.neutralTextColor
        font: Kirigami.Theme.smallFont
    }
}
