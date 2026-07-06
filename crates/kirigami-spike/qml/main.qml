// THROWAWAY SPIKE QML. A Kirigami.ApplicationWindow whose page shows a
// ListView backed by the Rust QAbstractListModel, plus a label that updates
// from the async `connectionAdded` signal.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.spike

Kirigami.ApplicationWindow {
    id: root
    title: "Snitchwatch cxx-qt/Kirigami spike"
    width: 640
    height: 480

    // Proof point 2: a QAbstractListModel instance backing a QML ListView.
    ConnectionModel {
        id: connectionModel
        onConnectionAdded: function (process, destination) {
            // Proof point 1: this fires from a signal emitted on the Qt thread
            // after a background Tokio task queued the append.
            lastEventLabel.text = "async signal: " + process + " -> " + destination
            console.log("PROOF1 async signal from Tokio->Qt:", process, "->", destination,
                        "| PROOF2 model rowCount =", connectionModel.count)
            if (connectionModel.count >= 5) {
                console.log("SPIKE OK: 5 rows delivered via QAbstractListModel; quitting")
                quitTimer.start()
            }
        }
    }

    // Give the async feed time to complete, then exit cleanly (headless CI).
    Timer {
        id: quitTimer
        interval: 400
        onTriggered: Qt.quit()
    }

    // Safety net so the process never hangs if signals never arrive.
    Timer {
        interval: 8000
        running: true
        onTriggered: Qt.quit()
    }

    Component.onCompleted: connectionModel.startFeed()

    pageStack.initialPage: Kirigami.Page {
        title: "Connections"

        ColumnLayout {
            anchors.fill: parent
            spacing: Kirigami.Units.largeSpacing

            Controls.Label {
                id: lastEventLabel
                text: "waiting for async feed..."
                Layout.fillWidth: true
            }

            Controls.Label {
                text: "rows: " + connectionModel.count
                Layout.fillWidth: true
            }

            Controls.ScrollView {
                Layout.fillWidth: true
                Layout.fillHeight: true

                ListView {
                    model: connectionModel
                    delegate: Controls.ItemDelegate {
                        id: row
                        width: ListView.view ? ListView.view.width : implicitWidth
                        required property string process
                        required property string destination
                        contentItem: ColumnLayout {
                            Controls.Label { text: row.process; font.bold: true }
                            Controls.Label { text: row.destination; opacity: 0.7 }
                        }
                    }
                }
            }
        }
    }
}
