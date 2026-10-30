// Recommended background-service rules (prompt-slot plan Part D; owner
// decision S3: opt-in, programs under /usr only, each limited to one host
// or this computer; S6: the one exception is the system resolver's DNS,
// any address on port 53, which "Turn all on" skips: the bridge marks it
// `broad`, and it is turned on by itself).
//
// Each row says exactly what its rule allows, in the bridge's words from
// its reviewed list, and why it is offered. Nothing is on unless the user
// turns it on: the switch follows `isOn`, which only the bridge sets, and
// a switch click only asks (`CuratedDefaultsModel.setEntry`). "Rule
// installed" shows only after the firewall service accepted the rule.
// Statuses are as of the firewall service's last rule list. A rule edited
// outside Snitchwatch can be removed here, only after a confirmation.
// So can a rule under Snitchwatch's reserved name that is no longer
// recommended and that Snitchwatch kept no copy of (#113, "No longer
// recommended"): the Rules page refuses those names, so this is the only way.
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
    // Rules no longer recommended (#113), from the bridge; [] from an older one.
    readonly property var orphans: {
        const json = page.model ? page.model.orphansJson : "";
        if (json.length === 0) return [];
        try { return JSON.parse(json); } catch (e) { return []; }
    }

    property alias dnsConsentDialog: dnsConsent
    DnsConsentDialog { id: dnsConsent; consentModel: page.model }
    onVisibleChanged: if (!visible && model) model.cancelBroadConsent()
    Component.onDestruction: if (model) model.cancelBroadConsent()

    header: ColumnLayout {
        spacing: 0

        Kirigami.InlineMessage {
            objectName: "explanation"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Information
            visible: true
            text: "Background services on this computer that need the network. Each rule lets "
                + "one program reach one host, or this computer, on one port, and says exactly "
                + "what it allows, except the DNS rule, which lets the system resolver reach "
                + "any address on port 53. Snitchwatch adds none unless you turn it on. The "
                + "\"Turn all on\" button skips the DNS rule, which you turn on by itself; "
                + "\"Turn all off\" turns it off too. A rule already in the firewall (added "
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
        Controls.Label {
            objectName: "dnsUnsupported"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            visible: page.model && !page.model.broadSupported
            text: "This service cannot accept reviewed DNS consent. Connect to an updated service to turn on or keep the DNS rule. Turning it off is still available."
        }
        Controls.Label {
            objectName: "dnsChoiceResult"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            visible: page.model && page.model.consentResult.length > 0
            text: page.model ? page.model.consentResult : ""
        }
    }

    FocusOrderListView {
        id: list
        model: page.model
        // Inside the list, so Tab reaches these before the rows (the page's
        // own header comes after the list in the focus order).
        header: RowLayout {
            width: list.width
            spacing: Kirigami.Units.largeSpacing
            visible: page.usable && list.count > 0
            height: visible ? implicitHeight : 0
            GuardedButton {
                objectName: "allOnButton"
                text: "Turn all on"
                Layout.margins: Kirigami.Units.smallSpacing
                onClicked: page.model.setAll(true)
            }
            GuardedButton {
                objectName: "allOffButton"
                text: "Turn all off"
                Layout.margins: Kirigami.Units.smallSpacing
                onClicked: page.model.setAll(false)
            }
        }
        // Under the bridge's group titles when it sends groups; the flat list otherwise.
        section.property: page.model && page.model.grouped ? "groupTitle" : ""
        section.criteria: ViewSection.FullString
        section.delegate: Kirigami.ListSectionHeader {
            required property string section
            width: ListView.view ? ListView.view.width : implicitWidth
            label: section
        }

        footer: ColumnLayout {
            objectName: "orphansSection"
            width: list.width
            visible: page.orphans.length > 0
            spacing: Kirigami.Units.smallSpacing

            Kirigami.ListSectionHeader {
                Layout.fillWidth: true
                label: "No longer recommended"
            }
            Controls.Label {
                objectName: "orphansExplanation"
                Layout.fillWidth: true
                Layout.margins: Kirigami.Units.smallSpacing
                wrapMode: Text.Wrap
                text: "These rules use a name reserved for Snitchwatch's recommended rules, but "
                    + "Snitchwatch no longer recommends them and kept no copy of them, so it "
                    + "won't remove them by itself. They can't be removed on the Rules page; "
                    + "remove them here."
            }
            Repeater {
                id: orphanRows
                objectName: "orphanRows"
                model: page.orphans
                delegate: ColumnLayout {
                    id: orphanRow
                    required property var modelData
                    Layout.fillWidth: true
                    Layout.margins: Kirigami.Units.smallSpacing
                    spacing: Kirigami.Units.smallSpacing
                    // Remove asks first; a new list from the bridge ends it.
                    property bool confirmingRemove: false

                    Controls.Label {
                        objectName: "orphanName"
                        textFormat: Text.PlainText
                        text: orphanRow.modelData.ruleName
                        font.bold: true
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        objectName: "orphanStatus"
                        textFormat: Text.PlainText
                        text: orphanRow.modelData.statusText
                        wrapMode: Text.Wrap
                        font: Kirigami.Theme.smallFont
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        objectName: "orphanProblem"
                        visible: orphanRow.modelData.problem.length > 0
                        textFormat: Text.PlainText
                        text: orphanRow.modelData.problem
                        wrapMode: Text.Wrap
                        color: Kirigami.Theme.negativeTextColor
                        font: Kirigami.Theme.smallFont
                        Layout.fillWidth: true
                    }
                    GuardedButton {
                        objectName: "orphanRemoveButton"
                        visible: orphanRow.modelData.canRemove && !orphanRow.confirmingRemove
                        text: "Remove…"
                        Accessible.name: "Remove old rule " + orphanRow.modelData.ruleName
                        onClicked: {
                            orphanRow.confirmingRemove = true;
                            orphanCancel.forceActiveFocus(Qt.TabFocusReason);
                        }
                    }
                    Controls.Label {
                        objectName: "orphanRemoveQuestion"
                        visible: orphanRow.modelData.canRemove && orphanRow.confirmingRemove
                        text: "Remove the firewall's rule under this name? Snitchwatch no longer "
                            + "recommends it, and it may allow or block something; see the Rules "
                            + "page."
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }
                    RowLayout {
                        visible: orphanRow.modelData.canRemove && orphanRow.confirmingRemove
                        GuardedButton {
                            objectName: "orphanRemoveConfirm"
                            text: "Remove rule"
                            Accessible.name: "Confirm removing old rule " + orphanRow.modelData.ruleName
                            onClicked: {
                                orphanRow.confirmingRemove = false;
                                page.model.removeOrphan(orphanRow.modelData.orphanId);
                            }
                        }
                        GuardedButton {
                            id: orphanCancel
                            objectName: "orphanRemoveCancel"
                            text: "Cancel"
                            Accessible.name: "Cancel removing old rule " + orphanRow.modelData.ruleName
                            onClicked: orphanRow.confirmingRemove = false
                        }
                    }
                }
            }
        }

        delegate: NavigableRow {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            hoverEnabled: false
            focusPolicy: Qt.StrongFocus
            Accessible.name: row.program
            Accessible.description: row.statusText

            required property string entryId
            required property string program
            required property string allows
            required property string why
            required property bool isBroad
            required property bool isOn
            required property string statusText
            required property string problem
            required property bool canRemove
            required property bool canKeep
            // The bridge holds this entry back here (its account unknown).
            required property bool heldBack
            // Remove asks first; a model reset (the bridge's answer) ends it.
            property bool confirmingRemove: false

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                GuardedSwitch {
                    objectName: "entrySwitch"
                    Accessible.name: "Recommended rule: " + row.program
                    enabled: page.usable && (!row.isBroad || row.isOn || page.model.broadSupported)
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
                        objectName: "problemLabel"
                        visible: row.problem.length > 0
                        textFormat: Text.PlainText
                        text: row.problem
                        wrapMode: Text.Wrap
                        color: Kirigami.Theme.negativeTextColor
                        font: Kirigami.Theme.smallFont
                        Layout.fillWidth: true
                    }
                    GuardedButton {
                        objectName: "keepButton"
                        Accessible.name: "Keep rule for " + row.program
                        visible: row.canKeep
                        enabled: !row.isBroad || page.model.broadSupported
                        text: "Keep"
                        onClicked: page.model.keepEntry(row.entryId)
                    }
                    GuardedButton {
                        objectName: "reviewDnsButton"
                        Accessible.name: "Review DNS choice for " + row.program
                        visible: row.isBroad && !row.canKeep && !row.canRemove
                        enabled: page.usable && page.model.broadSupported && !row.heldBack
                        text: "Review DNS choice…"
                        onClicked: page.model.setEntry(row.entryId, true)
                    }
                    GuardedButton {
                        objectName: "removeButton"
                        Accessible.name: "Remove rule for " + row.program
                        visible: row.canRemove && !row.confirmingRemove
                        text: "Remove…"
                        onClicked: {
                            row.confirmingRemove = true;
                            removeCancel.forceActiveFocus(Qt.TabFocusReason);
                        }
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
                        GuardedButton {
                            objectName: "removeConfirm"
                            Accessible.name: "Confirm removing rule for " + row.program
                            text: "Remove rule"
                            onClicked: {
                                row.confirmingRemove = false;
                                page.model.removeEntry(row.entryId);
                            }
                        }
                        GuardedButton {
                            id: removeCancel
                            objectName: "removeCancel"
                            Accessible.name: "Cancel removing rule for " + row.program
                            text: "Cancel"
                            onClicked: row.confirmingRemove = false
                        }
                    }
                }
            }
        }
    }
}
