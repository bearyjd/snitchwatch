// Connections list page (Task 8 — view layer).
//
// Binds a ListView to the Rust `ConnectionsModel` (Task 6). Row delegate shows
// process / host:port / protocol with a verdict marker: a hollow ◐ for pending
// rows, allow-green / deny-red for decided ones (per the design spec's
// pending-row styling). The inspector pane + search/filter + auto-select-on-
// new-pending-row behaviour from web/js/connections.js are follow-up work
// (tracked as Task 8 remaining); this establishes the delegate + list binding.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Connections"

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property ConnectionsModel model

    // Verdict token -> accent colour. Kept in QML since it's pure presentation.
    function verdictColor(verdict) {
        switch (verdict) {
        case "pending": return Kirigami.Theme.neutralTextColor;
        case "allowed": return Kirigami.Theme.positiveTextColor;
        case "denied": return Kirigami.Theme.negativeTextColor;
        case "blocklisted": return Kirigami.Theme.negativeTextColor;
        default: return Kirigami.Theme.disabledTextColor;
        }
    }

    function verdictGlyph(verdict, pending) {
        if (pending) return "◐";
        switch (verdict) {
        case "allowed": return "●";
        case "denied": return "●";
        case "blocklisted": return "⊘";
        default: return "○";
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: "network-connect"
        text: "No connections yet"
        explanation: "New connection prompts and recent decisions will appear here."
    }

    ListView {
        id: list
        model: page.model
        currentIndex: -1
        reuseItems: true

        delegate: Controls.ItemDelegate {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            highlighted: ListView.isCurrentItem

            required property int index
            required property string process
            required property string host
            required property int port
            required property string protocol
            required property string verdict
            required property bool pending

            onClicked: list.currentIndex = row.index

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                Controls.Label {
                    text: page.verdictGlyph(row.verdict, row.pending)
                    color: page.verdictColor(row.verdict)
                    Layout.alignment: Qt.AlignVCenter
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    Controls.Label {
                        text: row.process
                        font.bold: row.pending
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        text: row.host + ":" + row.port + "  " + row.protocol
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                }

                Controls.Label {
                    text: row.pending ? "pending" : row.verdict
                    color: page.verdictColor(row.verdict)
                    Layout.alignment: Qt.AlignVCenter
                }
            }
        }
    }
}
