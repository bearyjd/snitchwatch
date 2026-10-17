// The Rules page's rule detail sheet: the rule's state, the enable switch,
// Edit and the two-step delete. The page owns the `inspect*` state and the
// commands; `host` is the page.
//
// Rule detail + enable/disable + delete. Kept as an OverlaySheet, same as
// BlocklistsPage's inspector, so it behaves identically at every width.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

SizedOverlaySheet {
    id: inspector
    // The RulesPage that owns this sheet.
    property var host
    // Exposed for the headless inspector probe (tests/rules_inspector_qml.rs).
    property alias enabledSwitch: inspectEnabledSwitch
    property alias deleteButton: inspectDeleteButton
    property alias editButton: inspectEditButton
    property alias commandNote: inspectCommandNote
    title: host.inspectSource === "blocklist" ? ("blocklist: " + host.inspectBlocklistId) : host.inspectDisplayName

    ColumnLayout {
        Layout.preferredWidth: inspector.preferredWidth
        spacing: Kirigami.Units.largeSpacing

        Kirigami.FormLayout {
            Layout.fillWidth: true
            Controls.Label {
                Kirigami.FormData.label: "Name"
                textFormat: Text.PlainText
                text: host.inspectDisplayName
                elide: Text.ElideMiddle
            }
            Controls.Label {
                Kirigami.FormData.label: "Source"
                textFormat: Text.PlainText
                text: host.inspectSourceLabel
            }
            Controls.Label {
                Kirigami.FormData.label: "Action"
                textFormat: Text.PlainText
                text: host.inspectAction
                color: host.actionColor(host.inspectAction)
            }
            Controls.Label {
                Kirigami.FormData.label: "Duration"
                textFormat: Text.PlainText
                text: host.inspectDuration
            }
            Controls.Label {
                Kirigami.FormData.label: "Target"
                visible: host.inspectOperatorSummary.length > 0
                textFormat: Text.PlainText
                text: host.inspectOperatorSummary
                wrapMode: Text.Wrap
            }
            Controls.Label {
                objectName: "inspectHowItDecides"
                Kirigami.FormData.label: "Order"
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                text: "Position " + (host.inspectPrecedence + 1) + " of "
                      + (host.model ? host.model.count : 0) + " in the order the firewall "
                      + "checks rules (turned-off rules are counted, but skipped). "
                      + host.inspectHowItDecides
            }
            Controls.Switch {
                id: inspectEnabledSwitch
                Kirigami.FormData.label: "Enabled"
                enabled: host.inspectToggleable && (!host.inspectManagedDns || host.inspectEnabled)
                checked: host.inspectEnabled
                onToggled: host.setInspectEnabled(checked)
            }
            // The bridge's note on this rule's last toggle (#152).
            Controls.Label {
                id: inspectCommandNote
                objectName: "inspectCommandNote"
                visible: text.length > 0
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                text: host.inspectCommandNote
            }
            Controls.Button {
                objectName: "reenableDnsButton"
                visible: host.inspectManagedDns && !host.inspectEnabled && host.inspectToggleable
                enabled: host.curatedModel && host.curatedModel.broadSupported
                text: "Re-enable DNS…"
                onClicked: host.curatedModel.reenableDns()
            }
            Controls.Label {
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                Layout.fillWidth: true
                visible: host.inspectManagedDns && host.curatedModel && host.curatedModel.consentResult.length > 0
                text: host.curatedModel ? host.curatedModel.consentResult : ""
            }
            Controls.Label {
                Kirigami.FormData.label: "Read-only"
                visible: host.inspectReadOnlyReason.length > 0
                textFormat: Text.PlainText
                text: host.inspectReadOnlyReason
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
        }

        Kirigami.Separator {
            Layout.fillWidth: true
        }

        Controls.Button {
            id: inspectEditButton
            objectName: "inspectEditButton"
            Layout.fillWidth: true
            visible: !host.confirmingDelete
            enabled: host.inspectNotEditable.length === 0
            text: "Edit…"
            icon.name: "document-edit"
            onClicked: host.editInspected()
        }
        // A read-only rule already says why above.
        Controls.Label {
            objectName: "inspectNotEditable"
            visible: host.inspectReadOnlyReason.length === 0 && host.inspectNotEditable.length > 0
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: host.inspectNotEditable
            wrapMode: Text.Wrap
        }

        // Two-step confirmation kept inline (no separate dialog type
        // introduced) — mirrors the sheet's existing button-row pattern.
        Controls.Button {
            id: inspectDeleteButton
            Layout.fillWidth: true
            visible: !host.confirmingDelete
            enabled: host.inspectDeletable
            text: "Delete rule"
            icon.name: "edit-delete-remove"
            onClicked: host.confirmingDelete = true
        }

        ColumnLayout {
            Layout.fillWidth: true
            visible: host.confirmingDelete
            spacing: Kirigami.Units.smallSpacing

            Controls.Label {
                Layout.fillWidth: true
                text: "Delete this rule permanently?"
                color: Kirigami.Theme.negativeTextColor
                wrapMode: Text.Wrap
            }
            RowLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.largeSpacing
                Controls.Button {
                    Layout.fillWidth: true
                    text: "Cancel"
                    onClicked: host.confirmingDelete = false
                }
                Controls.Button {
                    Layout.fillWidth: true
                    text: "Confirm delete"
                    icon.name: "edit-delete-remove"
                    onClicked: {
                        host.model.deleteRule(host.inspectName);
                        host.confirmingDelete = false;
                        inspector.close();
                    }
                }
            }
        }
    }
}
