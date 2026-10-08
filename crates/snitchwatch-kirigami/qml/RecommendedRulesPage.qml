// Recommended background-service rules (prompt-slot plan Part D; owner
// decision S3: opt-in, programs under /usr only, each limited to one host
// or this computer).
//
// Each row says exactly what its rule allows, in the bridge's words from
// its reviewed list, and why it is offered. Nothing is on unless the user
// turns it on: the switch follows `isOn`, which only the bridge sets, and
// a switch click only asks (`CuratedDefaultsModel.setEntry`). "Rule
// installed" shows only after the firewall service accepted the rule.
// Statuses are as of the firewall service's last rule list. A rule edited
// outside Snitchwatch can be removed here, only after a confirmation.
//
// Every text from the bridge goes in a PlainText label; the warnings are
// fixed text (issue #51).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Recommended background-service rules"

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property CuratedDefaultsModel model

    readonly property bool received: page.model ? page.model.received : false
    readonly property string unavailableReason: page.model ? page.model.unavailableReason : ""
    // The switches only ask the bridge; they work while it adds these rules.
    readonly property bool usable: page.received && page.unavailableReason.length === 0
    property alias entriesList: list

    header: ColumnLayout {
        spacing: 0

        Kirigami.InlineMessage {
            objectName: "explanation"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Information
            visible: true
            text: "Background services on this computer that need the network. Each rule lets "
                + "one program reach one place, and says exactly what it allows. Snitchwatch "
                + "adds none unless you turn it on; a rule already in the firewall (added "
                + "earlier) stays as it is until you keep it or turn it off. What each says is "
                + "as of the firewall service's last rule list."
        }
        Kirigami.InlineMessage {
            objectName: "notOfferedBanner"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: !page.received
            text: "This Snitchwatch service hasn't sent its recommended rules. It may be an "
                + "older version that doesn't have them."
        }
        Kirigami.InlineMessage {
            objectName: "unavailableBanner"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: page.received && page.unavailableReason.length > 0
            text: "This Snitchwatch service doesn't add or remove recommended rules. Nothing "
                + "here can be turned on or off; rules already in the firewall stay as they are."
        }
        Controls.Label {
            objectName: "unavailableReason"
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            visible: page.unavailableReason.length > 0
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: page.unavailableReason
        }
        RowLayout {
            Layout.margins: Kirigami.Units.smallSpacing
            spacing: Kirigami.Units.largeSpacing
            visible: page.usable && list.count > 0
            Controls.Button {
                objectName: "allOnButton"
                text: "Turn all on"
                onClicked: page.model.setAll(true)
            }
            Controls.Button {
                objectName: "allOffButton"
                text: "Turn all off"
                onClicked: page.model.setAll(false)
            }
        }
    }

    ListView {
        id: list
        model: page.model

        delegate: Controls.ItemDelegate {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            hoverEnabled: false

            required property int index
            required property string entryId
            required property string program
            required property string allows
            required property string why
            required property bool isOn
            required property string statusText
            required property string problem
            required property bool canRemove
            required property bool canKeep
            // Remove asks first; a model reset (the bridge's answer) ends it.
            property bool confirmingRemove: false

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                Controls.Switch {
                    objectName: "entrySwitch"
                    enabled: page.usable
                    checked: row.isOn
                    Layout.alignment: Qt.AlignTop
                    onToggled: {
                        page.model.setEntry(row.entryId, checked);
                        // Back to the bridge's value; its next message moves it.
                        checked = Qt.binding(function () { return row.isOn; });
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Label {
                        textFormat: Text.PlainText
                        text: row.program
                        font.bold: true
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        objectName: "allowsLabel"
                        textFormat: Text.PlainText
                        text: row.allows
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        textFormat: Text.PlainText
                        text: row.why
                        wrapMode: Text.Wrap
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        objectName: "statusLabel"
                        textFormat: Text.PlainText
                        text: row.statusText
                        wrapMode: Text.Wrap
                        font: Kirigami.Theme.smallFont
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        visible: row.problem.length > 0
                        textFormat: Text.PlainText
                        text: row.problem
                        wrapMode: Text.Wrap
                        color: Kirigami.Theme.negativeTextColor
                        font: Kirigami.Theme.smallFont
                        Layout.fillWidth: true
                    }
                    Controls.Button {
                        objectName: "keepButton"
                        visible: row.canKeep
                        text: "Keep"
                        onClicked: page.model.keepEntry(row.entryId)
                    }
                    Controls.Button {
                        objectName: "removeButton"
                        visible: row.canRemove && !row.confirmingRemove
                        text: "Remove…"
                        onClicked: row.confirmingRemove = true
                    }
                    Controls.Label {
                        objectName: "removeQuestion"
                        visible: row.canRemove && row.confirmingRemove
                        text: "Remove the firewall's rule under this name? It differs from this "
                            + "description and may allow or block something else; see the Rules "
                            + "page."
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }
                    RowLayout {
                        visible: row.canRemove && row.confirmingRemove
                        Controls.Button {
                            objectName: "removeConfirm"
                            text: "Remove rule"
                            onClicked: {
                                row.confirmingRemove = false;
                                page.model.removeEntry(row.entryId);
                            }
                        }
                        Controls.Button {
                            objectName: "removeCancel"
                            text: "Cancel"
                            onClicked: row.confirmingRemove = false
                        }
                    }
                }
            }
        }
    }
}
