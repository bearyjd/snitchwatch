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
            text: page.model.troubleshootingText
        }

        Controls.Label {
            Layout.fillWidth: true
            visible: !page.model.hasProblem
            text: "opensnitchd is reachable, its firewall is running, and this \
                   host's kernel supports eBPF process monitoring and \
                   nftables — everything opensnitchd needs."
        }

        Controls.Button {
            text: "Recheck"
            onClicked: page.model.recheck()
        }
    }
}
