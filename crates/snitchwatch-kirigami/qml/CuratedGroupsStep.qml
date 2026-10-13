// Onboarding step after the silent-mode choice: offer the bridge's curated
// rule groups, one switch per group, all off until the user turns one on
// (opt-in). A switch only asks (`CuratedDefaultsModel.setGroup`, one request
// with that group's entry ids); it shows the bridge's answer, not the click.
// The name-resolution group allows any address on port 53, so its switch goes
// through the same reviewed DNS consent dialog as the Recommended rules page.
// Every text from the bridge is PlainText. Skipping sends nothing.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: step
    spacing: Kirigami.Units.largeSpacing

    // Injected by the caller (main.qml): the CuratedDefaultsModel.
    property CuratedDefaultsModel model

    readonly property bool usable: step.model
        ? step.model.received && step.model.unavailableReason.length === 0 : false
    readonly property var groups: {
        if (!step.model || !step.model.grouped) {
            return [];
        }
        try {
            return JSON.parse(step.model.groupsJson);
        } catch (e) {
            return [];
        }
    }
    readonly property bool anyOn: step.groups.some(g => g.isOn)
    property alias groupsRepeater: repeater
    property alias dnsConsentDialog: dnsConsent
    // The user is done with this step (skipped, or finished choosing).
    signal finished()

    DnsConsentDialog { id: dnsConsent; consentModel: step.model }
    Component.onDestruction: if (step.model) step.model.cancelBroadConsent()

    Kirigami.Heading {
        Layout.fillWidth: true
        level: 2
        wrapMode: Text.WordWrap
        text: "Recommended rules"
    }

    Controls.Label {
        objectName: "groupsIntro"
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        text: "Optional. Each switch adds a few rules for background programs. Each rule "
            + "lets one program reach the place listed under Details, and nothing else "
            + "(except Name resolution, which can reach any DNS server and asks you first). "
            + "Everything is off until you turn it on, and you can change it later under "
            + "Recommended background-service rules."
    }

    Kirigami.InlineMessage {
        objectName: "groupsUnavailable"
        Layout.fillWidth: true
        type: Kirigami.MessageType.Warning
        visible: step.model && step.model.received && step.model.unavailableReason.length > 0
        text: "This Snitchwatch service doesn't add recommended rules, so nothing here can be "
            + "turned on."
    }
    Controls.Label {
        objectName: "dnsUnsupported"
        Layout.fillWidth: true
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        visible: step.model && !step.model.broadSupported
        text: "This service cannot accept reviewed DNS consent, so the name-resolution rule "
            + "cannot be turned on from here."
    }
    Controls.Label {
        objectName: "dnsChoiceResult"
        Layout.fillWidth: true
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        visible: step.model && step.model.consentResult.length > 0
        text: step.model ? step.model.consentResult : ""
    }

    Repeater {
        id: repeater
        model: step.groups
        delegate: ColumnLayout {
            id: groupRow
            required property var modelData
            property bool expanded: false
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.largeSpacing

                Controls.Switch {
                    objectName: "groupSwitch-" + groupRow.modelData.id
                    Layout.alignment: Qt.AlignTop
                    enabled: step.usable
                        && (!groupRow.modelData.isBroad || groupRow.modelData.isOn
                            || step.model.broadSupported)
                    checked: groupRow.modelData.isOn
                    onToggled: {
                        step.model.setGroup(groupRow.modelData.id, checked);
                        // Back to the bridge's value; its next message moves it.
                        checked = Qt.binding(function () { return groupRow.modelData.isOn; });
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Controls.Label {
                        textFormat: Text.PlainText
                        text: groupRow.modelData.title
                        font.bold: true
                        Layout.fillWidth: true
                        wrapMode: Text.Wrap
                    }
                    Controls.Label {
                        objectName: "groupDescription-" + groupRow.modelData.id
                        textFormat: Text.PlainText
                        text: groupRow.modelData.description
                        wrapMode: Text.Wrap
                        opacity: 0.7
                        Layout.fillWidth: true
                    }
                    Controls.Button {
                        objectName: "groupDetails-" + groupRow.modelData.id
                        flat: true
                        text: groupRow.expanded ? "Hide details" : "Details"
                        onClicked: groupRow.expanded = !groupRow.expanded
                    }
                    Repeater {
                        model: groupRow.expanded ? groupRow.modelData.entries : []
                        delegate: ColumnLayout {
                            required property var modelData
                            Layout.fillWidth: true
                            Layout.leftMargin: Kirigami.Units.largeSpacing
                            spacing: 0
                            Controls.Label {
                                objectName: "entryProgram"
                                textFormat: Text.PlainText
                                text: modelData.program
                                elide: Text.ElideMiddle
                                Layout.fillWidth: true
                            }
                            Controls.Label {
                                objectName: "entryAllows"
                                textFormat: Text.PlainText
                                text: modelData.allows
                                wrapMode: Text.Wrap
                                Layout.fillWidth: true
                            }
                            Controls.Label {
                                textFormat: Text.PlainText
                                text: modelData.why
                                wrapMode: Text.Wrap
                                opacity: 0.7
                                font: Kirigami.Theme.smallFont
                                Layout.fillWidth: true
                            }
                            Controls.Label {
                                textFormat: Text.PlainText
                                text: modelData.statusText
                                wrapMode: Text.Wrap
                                font: Kirigami.Theme.smallFont
                                Layout.fillWidth: true
                            }
                        }
                    }
                }
            }
        }
    }

    Controls.Button {
        objectName: "groupsFinish"
        Layout.alignment: Qt.AlignRight
        text: step.anyOn ? "Continue" : "Skip"
        icon.name: "go-next"
        onClicked: step.finished()
    }
}
