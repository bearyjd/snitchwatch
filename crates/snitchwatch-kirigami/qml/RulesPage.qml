// Rules tab (Task 10 — view layer over RulesModel).
//
// Same list/detail shape as BlocklistsPage.qml (Task 9): a ListView bound to
// `RulesModel`, with a Kirigami.OverlaySheet detail view for enable/disable +
// delete + precedence display.
//
// Grouping: per the design doc's "blocklist verdict type" section, every
// subscribed blocklist entry is materialized into a deny rule in the
// `900-blocklist:<id>:` filename band. Those rules are the SAME underlying
// deny rules already shown in full (per-host) on the Blocklists tab, so this
// page groups them into their own "Blocklist rules" section — via
// ListView.section keyed on the model's `source` role — rendered visually
// muted (reduced opacity, no operator-summary line) rather than repeating
// per-host detail that would just confuse the two tabs' purposes. User rules
// always evaluate first regardless (see the design doc's specificity
// section), independent of this display grouping.
//
// toggleEnabled/deleteRule are plain qinvokables on `RulesModel`; they emit
// `ruleChangeRequested` with a JSON-encoded `ClientMessage` for the live
// bridge feed to forward — the same signal-out pattern
// `BlocklistsModel.subscribe`/`unsubscribe` and `PendingDecision.submit` use
// (no bridge changes, no local optimistic mutation — the row reflects the
// server's next `SetRules`/`UpdateRules` push).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Rules"

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property RulesModel model

    // Snapshot of the rule currently shown in the detail sheet.
    property string inspectName: ""
    property bool inspectEnabled: true
    property string inspectAction: ""
    property string inspectDuration: ""
    property string inspectOperatorSummary: ""
    property int inspectPrecedence: 0
    property string inspectSource: "user"
    property string inspectBlocklistId: ""
    property bool confirmingDelete: false

    function actionColor(action) {
        return action === "allow" ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.negativeTextColor;
    }

    function sourceLabel(source) {
        return source === "blocklist" ? "Blocklist rules" : "User rules";
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: "view-list-details"
        text: "No rules yet"
        explanation: "Rules created from connection decisions or the Blocklists tab will appear here."
    }

    ListView {
        id: list
        model: page.model
        currentIndex: -1
        reuseItems: true

        section.property: "source"
        section.criteria: ViewSection.FullString
        section.delegate: Kirigami.ListSectionHeader {
            width: ListView.view ? ListView.view.width : implicitWidth
            text: page.sourceLabel(section)
        }

        delegate: Controls.ItemDelegate {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            highlighted: ListView.isCurrentItem
            opacity: row.source === "blocklist" ? 0.7 : 1.0

            required property int index
            required property string name
            required property bool enabled
            // Named `ruleAction` (not `action`) because `Controls.ItemDelegate`
            // (an `AbstractButton` subclass) already declares a built-in
            // `action` property for binding a `QQuickAction` — reusing that
            // name here silently breaks delegate component creation with no
            // diagnostic. See `RulesModel::role_names`'s matching comment.
            required property string ruleAction
            required property string duration
            required property string operatorSummary
            required property int precedence
            required property string source
            required property string blocklistId

            onClicked: {
                list.currentIndex = row.index;
                page.openInspector(row);
            }

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                Controls.Label {
                    text: row.enabled ? "●" : "○"
                    color: row.enabled ? page.actionColor(row.ruleAction) : Kirigami.Theme.disabledTextColor
                    Layout.alignment: Qt.AlignVCenter
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    Controls.Label {
                        text: row.source === "blocklist" ? ("blocklist: " + row.blocklistId) : row.name
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    // Blocklist-sourced rows skip the per-host operator
                    // summary — that detail already lives on the
                    // Blocklists tab; repeating it here would just be noise.
                    Controls.Label {
                        visible: row.source !== "blocklist" && row.operatorSummary.length > 0
                        text: row.operatorSummary
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                }

                Controls.Label {
                    text: "#" + row.precedence
                    opacity: 0.6
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    text: row.ruleAction
                    color: page.actionColor(row.ruleAction)
                    Layout.alignment: Qt.AlignVCenter
                }
            }
        }
    }

    function openInspector(row) {
        page.inspectName = row.name;
        page.inspectEnabled = row.enabled;
        page.inspectAction = row.ruleAction;
        page.inspectDuration = row.duration;
        page.inspectOperatorSummary = row.operatorSummary;
        page.inspectPrecedence = row.precedence;
        page.inspectSource = row.source;
        page.inspectBlocklistId = row.blocklistId;
        page.confirmingDelete = false;
        inspector.open();
    }

    // Rule detail + enable/disable + delete. Kept as an OverlaySheet, same as
    // BlocklistsPage's inspector, so it behaves identically at every width.
    Kirigami.OverlaySheet {
        id: inspector
        title: page.inspectSource === "blocklist" ? ("blocklist: " + page.inspectBlocklistId) : page.inspectName

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            Kirigami.FormLayout {
                Layout.fillWidth: true
                Controls.Label {
                    Kirigami.FormData.label: "Name"
                    text: page.inspectName
                    elide: Text.ElideMiddle
                }
                Controls.Label {
                    Kirigami.FormData.label: "Source"
                    text: page.sourceLabel(page.inspectSource)
                }
                Controls.Label {
                    Kirigami.FormData.label: "Action"
                    text: page.inspectAction
                    color: page.actionColor(page.inspectAction)
                }
                Controls.Label {
                    Kirigami.FormData.label: "Duration"
                    text: page.inspectDuration
                }
                Controls.Label {
                    Kirigami.FormData.label: "Target"
                    visible: page.inspectOperatorSummary.length > 0
                    text: page.inspectOperatorSummary
                    wrapMode: Text.Wrap
                }
                Controls.Label {
                    Kirigami.FormData.label: "Precedence"
                    text: "Position " + (page.inspectPrecedence + 1) + " of " + (page.model ? page.model.count : 0)
                          + " — evaluated in this order, first match wins"
                }
                Controls.Switch {
                    Kirigami.FormData.label: "Enabled"
                    checked: page.inspectEnabled
                    onToggled: {
                        page.model.toggleEnabled(page.inspectName);
                        page.inspectEnabled = checked;
                    }
                }
            }

            Kirigami.Separator {
                Layout.fillWidth: true
            }

            // Two-step confirmation kept inline (no separate dialog type
            // introduced) — mirrors the sheet's existing button-row pattern.
            Controls.Button {
                Layout.fillWidth: true
                visible: !page.confirmingDelete
                text: "Delete rule"
                icon.name: "edit-delete-remove"
                onClicked: page.confirmingDelete = true
            }

            ColumnLayout {
                Layout.fillWidth: true
                visible: page.confirmingDelete
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
                        onClicked: page.confirmingDelete = false
                    }
                    Controls.Button {
                        Layout.fillWidth: true
                        text: "Confirm delete"
                        icon.name: "edit-delete-remove"
                        onClicked: {
                            page.model.deleteRule(page.inspectName);
                            page.confirmingDelete = false;
                            inspector.close();
                        }
                    }
                }
            }
        }
    }
}
