// A program's friendly label (`app_names.rs`) as the Connections list shows it.
// Display only: group keys stay path-based. A label that cannot be trusted
// (`programUnverified`: from the path's shape, a desktop file, or altered)
// gets a " ?" marker, the tail of the real path beside it and the full real
// path in a tooltip, because a program controls its own path and name.
// All text is data, so every label is PlainText.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

RowLayout {
    id: part

    // The ConnectionRow delegate: its programLabel, programLabelUnverified and
    // programPathTail roles are what is shown.
    property var src
    readonly property bool programUnverified: part.src ? part.src.programLabelUnverified : false
    // The real path for the tooltip, with unsafe characters shown as escapes.
    readonly property string fullPath: part.src ? part.src.programPathFull : ""
    property bool bold: false
    property color labelColor: Kirigami.Theme.textColor

    spacing: Kirigami.Units.smallSpacing

    Controls.Label {
        textFormat: Text.PlainText
        text: part.src.programLabel + (part.programUnverified ? " ?" : "")
        font.bold: part.bold
        color: part.labelColor
        elide: Text.ElideRight
        Layout.fillWidth: true
        HoverTip {
            text: part.fullPath
            onlyWhen: part.programUnverified
        }
    }
    Controls.Label {
        visible: part.programUnverified
        textFormat: Text.PlainText
        text: part.src.programPathTail
        opacity: 0.7
        font: Kirigami.Theme.smallFont
        elide: Text.ElideMiddle
        Layout.maximumWidth: Kirigami.Units.gridUnit * 12
        HoverTip {
            text: part.fullPath
            onlyWhen: part.programUnverified
        }
    }
}
