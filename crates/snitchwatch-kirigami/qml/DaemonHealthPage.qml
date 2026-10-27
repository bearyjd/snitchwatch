import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: page
    title: "Daemon Health"

    required property var model

    ColumnLayout {
        width: page.width
        spacing: Kirigami.Units.largeSpacing

        Kirigami.InlineMessage {
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: page.model.hasProblem
            text: page.model.statusSummary
        }

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            visible: page.model.hasProblem
            // Failure details from the bridge's daemon and kernel checks
            // (issue #51).
            textFormat: Text.PlainText
            objectName: "daemonHealthTroubleshooting"
            text: page.model.troubleshootingText
        }

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            visible: !page.model.hasProblem
            objectName: "daemonHealthStatus"
            text: page.model.statusSummary
        }

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            objectName: "processMonitorReported"
            textFormat: Text.PlainText
            text: page.model.monitorStatus
        }

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            objectName: "processMonitorSupport"
            text: "The GUI supports both /proc and eBPF process monitoring. \
                   opensnitchd does not report the active method to the GUI; \
                   check the daemon log to confirm which method is running."
        }

        Controls.Button {
            text: "Recheck"
            onClicked: page.model.recheck()
        }
    }
}
