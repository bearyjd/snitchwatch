// Rules tab (Task 10 — view layer over RulesModel).
//
// Same list/detail shape as BlocklistsPage.qml (Task 9): a ListView bound to
// `RulesModel`, with a Kirigami.OverlaySheet detail view for enable/disable +
// delete + precedence display.
//
// Grouping: each subscribed blocklist is one deny rule per list kind
// (`z00-blocklist:<id>:domains`, and `…:ips` for lists with IP addresses),
// reading the list's hosts from a file (issue #45). Their hosts are shown on
// the Blocklists tab, so this page groups the rules into their own
// "Blocklist rules" section — via ListView.section keyed on the model's
// `source` role — rendered visually muted (reduced opacity, no
// operator-summary line). They are read-only here ("Managed on the
// Blocklists page", from the bridge's `readOnlyReason`) and can't be
// deleted here. A matching blocklist deny wins over every allow that isn't
// a precedence rule, whatever the order shown.
//
// setEnabled/deleteRule are plain qinvokables on `RulesModel`; they emit
// `ruleChangeRequested` with a JSON-encoded `ClientMessage` for the live
// bridge feed to forward — the same signal-out pattern
// `BlocklistsModel.subscribe`/`unsubscribe` uses (no local mutation of the
// model — the row reflects the server's next `SetRules`/`UpdateRules` push).
// Only the inspector's switch moves ahead of the bridge, and the inspector
// re-reads its rule on every model reset (#48).
//
// Names are shown via the `displayName` role (bidi overrides and zero-width
// characters removed by the bridge); `name` stays the rule's identity.
//
// Issue #44: rules earlier Snitchwatch versions saved for "This host" / "Any
// host on this domain" match every program (`rules::all_apps`). Each such row
// is flagged with what deleting it changes and its own Delete button — one
// click, one rule. Deliberately no bulk delete: removing a deny can unblock
// traffic, so every deletion stays a deliberate, per-row choice.
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
    property string inspectDisplayName: ""
    // Non-empty when Snitchwatch can't edit the rule (the bridge's reason);
    // the rule is still shown because the daemon still enforces it.
    property string inspectReadOnlyReason: ""
    // A rule read-only only for its conditions can still be deleted.
    property bool inspectDeletable: false
    property bool inspectEnabled: true
    property string inspectAction: ""
    property string inspectDuration: ""
    property string inspectOperatorSummary: ""
    property int inspectPrecedence: 0
    property string inspectSource: "user"
    property string inspectBlocklistId: ""
    property bool confirmingDelete: false
    // Exposed for the headless inspector probe (tests/rules_inspector_qml.rs).
    property alias inspectorSheet: inspector
    property alias inspectorEnabledSwitch: inspectEnabledSwitch
    property alias inspectorDeleteButton: inspectDeleteButton
    property alias rulesList: list

    function actionColor(action) {
        return action === "allow" ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.negativeTextColor;
    }

    function sourceLabel(source) {
        return source === "blocklist" ? "Blocklist rules" : "User rules";
    }

    // Rule-match diagnostics' "Show rule" jump target (called by main.qml
    // after navigating here from ConnectionsPage's inspector). Re-populates
    // the same inspect* properties `openInspector` uses — `selectRuleByName`
    // returns the identical JSON shape — and opens the detail sheet directly,
    // without needing to locate/scroll the row in the ListView first.
    // Returns whether a rule by that name was found.
    function openRuleByName(name) {
        if (!page.model) return false;
        const json = page.model.selectRuleByName(name);
        if (!json) return false;
        const rule = JSON.parse(json);
        page.fillInspector(rule);
        page.confirmingDelete = false;
        list.currentIndex = rule.precedence;
        list.positionViewAtIndex(rule.precedence, ListView.Contain);
        inspector.open();
        return true;
    }

    // `rule` is `selectRuleByName`'s JSON shape.
    function fillInspector(rule) {
        page.inspectName = rule.name;
        page.inspectDisplayName = rule.displayName;
        page.inspectReadOnlyReason = rule.readOnlyReason;
        page.inspectDeletable = rule.deletable;
        page.inspectEnabled = rule.enabled;
        page.inspectAction = rule.action;
        page.inspectDuration = rule.duration;
        page.inspectOperatorSummary = rule.operatorSummary;
        page.inspectPrecedence = rule.precedence;
        page.inspectSource = rule.source;
        page.inspectBlocklistId = rule.blocklistId;
    }

    // The bridge re-sends the whole list after every rule command, whether
    // the daemon accepted it, refused it or never answered, and clears it
    // when its daemon stream goes away (#48). Re-read the open rule so the
    // sheet never keeps a state the daemon doesn't have; close the sheet if
    // the rule is gone.
    function refreshInspector() {
        if (!inspector.visible || !page.inspectName || !page.model) return;
        const json = page.model.selectRuleByName(page.inspectName);
        if (!json) {
            page.confirmingDelete = false;
            inspector.close();
            return;
        }
        page.fillInspector(JSON.parse(json));
    }

    // The switch sends the value it now shows, not a flip of the model's
    // (possibly not yet updated) value, then goes back to following
    // `inspectEnabled`, which the next model reset corrects.
    function setInspectEnabled(enabled) {
        if (!page.model) return;
        page.model.setEnabled(page.inspectName, enabled);
        page.inspectEnabled = enabled;
        inspectEnabledSwitch.checked = Qt.binding(function () { return page.inspectEnabled; });
    }

    Connections {
        target: page.model
        function onModelReset() {
            page.refreshInspector();
        }
    }

    // "Simulate" panel entry point (Little-Snitch-parity rule-match
    // diagnostics), kept in the header so it stays reachable regardless of
    // scroll position, same rationale as ConnectionsPage's search field.
    titleDelegate: RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Heading {
            text: page.title
            level: 1
            Layout.alignment: Qt.AlignVCenter
        }
        Item {
            Layout.fillWidth: true
        }
        Controls.Button {
            text: "Simulate"
            icon.name: "system-run"
            onClicked: simulateSheet.open()
        }
    }

    // Issue #44: only when some rules apply to every app. Fixed text; the
    // count sits in a PlainText label (InlineMessage can't render data).
    header: ColumnLayout {
        visible: !!page.model && page.model.legacyHostOnlyCount > 0
        spacing: 0

        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: true
            type: Kirigami.MessageType.Information
            text: "Some rules saved by earlier Snitchwatch versions apply to all apps, not only "
                + "the app that asked. They are marked below, each with what deleting it changes."
        }
        // Counts only Snitchwatch's own earlier rules: blocklist or
        // hand-written rules may apply to all apps too, so no "of N".
        Controls.Label {
            objectName: "allAppsCount"
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            text: !page.model ? ""
                : page.model.legacyHostOnlyCount === 1
                    ? "1 rule saved by an earlier Snitchwatch version applies to all apps"
                    : page.model.legacyHostOnlyCount
                      + " rules saved by earlier Snitchwatch versions apply to all apps"
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: "view-list-details"
        text: "No rules yet"
        explanation: "Decisions set to “This time” resolve only the current request. Choose a persistent duration, or add a blocklist, to create rules shown here."
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
            required property string displayName
            required property string readOnlyReason
            required property bool deletable
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
            required property bool appliesToAllApps
            required property string allAppsHint

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
                        textFormat: Text.PlainText
                        text: row.source === "blocklist" ? ("blocklist: " + row.blocklistId) : row.displayName
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    // Blocklist-sourced rows skip the per-host operator
                    // summary — that detail already lives on the
                    // Blocklists tab; repeating it here would just be noise.
                    Controls.Label {
                        visible: row.source !== "blocklist" && row.operatorSummary.length > 0
                        textFormat: Text.PlainText
                        text: row.operatorSummary
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                    // Issue #44: what deleting this all-apps rule changes.
                    Controls.Label {
                        objectName: "allAppsHint"
                        visible: row.appliesToAllApps
                        textFormat: Text.PlainText
                        text: row.allAppsHint
                        wrapMode: Text.Wrap
                        font: Kirigami.Theme.smallFont
                        color: row.ruleAction === "allow" ? Kirigami.Theme.neutralTextColor
                                                          : Kirigami.Theme.negativeTextColor
                        Layout.fillWidth: true
                    }
                }

                Controls.Label {
                    objectName: "allAppsFlag"
                    visible: row.appliesToAllApps
                    text: "Applies to all apps"
                    color: Kirigami.Theme.neutralTextColor
                    font.bold: true
                    Layout.alignment: Qt.AlignVCenter
                }

                // One click deletes this one rule; the hint above says what
                // that changes. No confirmation step, no bulk variant.
                Controls.Button {
                    objectName: "allAppsDelete"
                    visible: row.appliesToAllApps && row.deletable
                    text: "Delete"
                    icon.name: "edit-delete-remove"
                    Layout.alignment: Qt.AlignVCenter
                    onClicked: page.model.deleteRule(row.name)
                }

                Controls.Label {
                    visible: row.readOnlyReason.length > 0
                    text: "read-only"
                    opacity: 0.6
                    font: Kirigami.Theme.smallFont
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    text: "#" + row.precedence
                    opacity: 0.6
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    textFormat: Text.PlainText
                    text: row.ruleAction
                    color: page.actionColor(row.ruleAction)
                    Layout.alignment: Qt.AlignVCenter
                }
            }
        }
    }

    function openInspector(row) {
        page.inspectName = row.name;
        page.inspectDisplayName = row.displayName;
        page.inspectReadOnlyReason = row.readOnlyReason;
        page.inspectDeletable = row.deletable;
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
    SizedOverlaySheet {
        id: inspector
        title: page.inspectSource === "blocklist" ? ("blocklist: " + page.inspectBlocklistId) : page.inspectDisplayName

        ColumnLayout {
            Layout.preferredWidth: inspector.preferredWidth
            spacing: Kirigami.Units.largeSpacing

            Kirigami.FormLayout {
                Layout.fillWidth: true
                Controls.Label {
                    Kirigami.FormData.label: "Name"
                    textFormat: Text.PlainText
                    text: page.inspectDisplayName
                    elide: Text.ElideMiddle
                }
                Controls.Label {
                    Kirigami.FormData.label: "Source"
                    textFormat: Text.PlainText
                    text: page.sourceLabel(page.inspectSource)
                }
                Controls.Label {
                    Kirigami.FormData.label: "Action"
                    textFormat: Text.PlainText
                    text: page.inspectAction
                    color: page.actionColor(page.inspectAction)
                }
                Controls.Label {
                    Kirigami.FormData.label: "Duration"
                    textFormat: Text.PlainText
                    text: page.inspectDuration
                }
                Controls.Label {
                    Kirigami.FormData.label: "Target"
                    visible: page.inspectOperatorSummary.length > 0
                    textFormat: Text.PlainText
                    text: page.inspectOperatorSummary
                    wrapMode: Text.Wrap
                }
                Controls.Label {
                    Kirigami.FormData.label: "Precedence"
                    text: "Position " + (page.inspectPrecedence + 1) + " of " + (page.model ? page.model.count : 0)
                          + " — evaluated in this order, first match wins"
                }
                Controls.Switch {
                    id: inspectEnabledSwitch
                    Kirigami.FormData.label: "Enabled"
                    enabled: page.inspectReadOnlyReason.length === 0
                    checked: page.inspectEnabled
                    onToggled: page.setInspectEnabled(checked)
                }
                Controls.Label {
                    Kirigami.FormData.label: "Read-only"
                    visible: page.inspectReadOnlyReason.length > 0
                    textFormat: Text.PlainText
                    text: page.inspectReadOnlyReason
                    wrapMode: Text.Wrap
                    Layout.fillWidth: true
                }
            }

            Kirigami.Separator {
                Layout.fillWidth: true
            }

            // Two-step confirmation kept inline (no separate dialog type
            // introduced) — mirrors the sheet's existing button-row pattern.
            Controls.Button {
                id: inspectDeleteButton
                Layout.fillWidth: true
                visible: !page.confirmingDelete
                enabled: page.inspectDeletable
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

    // Rule-match simulator sheet, opened by the header's "Simulate" button.
    RuleSimulatorSheet {
        id: simulateSheet
        model: page.model
    }
}
