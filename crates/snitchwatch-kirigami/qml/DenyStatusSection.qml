// Deny-mode status from the tower helper's read-only snapshot (bazzite-tower
// #138; docs/superpowers/plans/2026-10-27-deny-status-display.md). Display
// only: the commands are for the user to copy and run themselves. There are
// no action buttons, and nothing here asks for privilege or starts a process.
import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: section
    required property var model
    spacing: Kirigami.Units.smallSpacing

    // The countdown and staleness are against the clock, so re-read while shown.
    Timer {
        interval: 1000
        repeat: true
        running: section.visible
        triggeredOnStart: true
        onTriggered: section.model.refresh()
    }
    ClipboardCopy { id: clipboard }

    Kirigami.Heading {
        level: 3
        text: "Deny mode (system firewall default)"
    }
    Controls.Label {
        objectName: "denyHeadline"
        Layout.fillWidth: true
        wrapMode: Text.WordWrap
        font.bold: section.model.state === "overdue"
        text: section.model.headline
    }
    Controls.Label {
        objectName: "denyDetail"
        Layout.fillWidth: true
        wrapMode: Text.WordWrap
        visible: text !== ""
        text: section.model.detail
    }
    Controls.Label {
        objectName: "denyAsOf"
        visible: text !== ""
        opacity: 0.7
        text: section.model.asOf
    }
    Controls.Label {
        // The helper's own blocker lines, which can carry config paths.
        objectName: "denyBlockers"
        Layout.fillWidth: true
        wrapMode: Text.WordWrap
        textFormat: Text.PlainText
        visible: text !== ""
        text: section.model.blockersText
    }
    Controls.Label {
        Layout.fillWidth: true
        wrapMode: Text.WordWrap
        text: "To change it, run one of these in a terminal (on [minutes] takes 1 to 60):"
    }
    Repeater {
        model: JSON.parse(section.model.commandsJson)
        RowLayout {
            required property string modelData
            Layout.fillWidth: true
            Controls.Label {
                Layout.fillWidth: true
                font.family: "monospace"
                textFormat: Text.PlainText
                text: modelData
            }
            Controls.Button {
                objectName: "copyDenyCommand"
                text: "Copy"
                Accessible.name: "Copy " + modelData
                onClicked: clipboard.copy(modelData)
            }
        }
    }
}
