// A group header's "N pending" badge: collapsed groups expose pending
// connections through it. Split out of ConnectionRow.qml for its line budget.
// Use a border: Fusion/Basic can give neutral background and text the same
// color, making a filled badge unreadable (issue #31).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Rectangle {
    id: badge

    property int count: 0

    color: "transparent"
    border.color: Kirigami.Theme.neutralTextColor
    border.width: 1
    radius: height / 2
    implicitWidth: pendingBadgeLabel.implicitWidth + Kirigami.Units.largeSpacing
    implicitHeight: pendingBadgeLabel.implicitHeight + Kirigami.Units.smallSpacing
    Layout.alignment: Qt.AlignVCenter

    Controls.Label {
        id: pendingBadgeLabel
        anchors.centerIn: parent
        textFormat: Text.PlainText
        text: badge.count + " pending"
        color: Kirigami.Theme.neutralTextColor
        font: Kirigami.Theme.smallFont
    }
}
