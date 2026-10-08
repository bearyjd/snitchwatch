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
// Hit counts (P2.6 Part 1): each row says how often its rule decided a
// connection, and the header says what those numbers are: Snitchwatch's own
// tally of the daemon's per-ping events, since a time, approximate, and
// possibly missing some. Nothing is shown until the bridge has sent counts
// (an older bridge never does), a rule that doesn't log, or whose name the
// bridge can't count, is "not counted" rather than 0, the count is a `real`
// (a QML int stops at 2^31 - 1), and the header says when the counts don't
// survive a restart. Every label is PlainText.
//
// Names are shown via the `displayName` role (bidi overrides and zero-width
// characters removed by the bridge); `name` stays the rule's identity.
//
// Issue #44: rules earlier Snitchwatch versions saved for "This host" / "Any
// host on this domain" match every program (`rules::all_apps`). Each such row
// is flagged with what deleting it changes and its own Delete button — one
// click, one rule. Deliberately no bulk delete: removing a deny can unblock
// traffic, so every deletion stays a deliberate, per-row choice.
//
// Rule import/export (roadmap P2.7): "Export…" saves the firewall's user
// rules through a file dialog; "Import…" reads a rules file and opens
// `RulesImportSheet` with the bridge's dry-run preview. The GUI reads and
// writes the files; the bridge checks every rule. Import never deletes.
// The buttons stay enabled while the list is empty: the bridge says when
// the firewall's rules haven't loaded (its answer shows below the title).
import QtQuick
import QtQuick.Dialogs
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
    // How the rule takes part in the daemon's decision (issue #102).
    property string inspectHowItDecides: ""
    property string inspectSource: "user"
    property string inspectBlocklistId: ""
    property bool confirmingDelete: false
    // Why the rule editor can't change the inspected rule; empty when it can.
    property string inspectNotEditable: ""
    // Exposed for the headless inspector probe (tests/rules_inspector_qml.rs).
    property alias inspectorSheet: inspector
    property alias inspectorEnabledSwitch: inspectEnabledSwitch
    property alias inspectorDeleteButton: inspectDeleteButton
    property alias rulesList: list
    property alias rulesIo: rulesIo
    property alias importSheet: importSheet
    property alias ruleEditor: ruleEditor
    property alias inspectorEditButton: inspectEditButton
    readonly property bool showsAllAppsNotice: !!page.model && page.model.legacyHostOnlyCount > 0
    // Not `ioStatus.visible`: a child of a hidden header always reads false.
    readonly property bool showsIoStatus: rulesIo.statusText.length > 0 && !importSheet.visible
    // The rule editor's last result once its sheet closed (P2.1).
    // What the list leaves out (issue #61).
    readonly property bool showsNotShown: !!page.model && page.model.notShownText.length > 0
    // "No rules yet", but not while the header says rules aren't listed.
    readonly property bool showsEmptyPlaceholder: (!page.model || page.model.count === 0)
                                                  && !page.showsNotShown
    readonly property bool showsEditorStatus: ruleEditorController.statusText.length > 0
                                              && !ruleEditor.visible

    function actionColor(action) {
        return action === "allow" ? Kirigami.Theme.positiveTextColor : Kirigami.Theme.negativeTextColor;
    }

    function sourceLabel(source) {
        if (source === "blocklist") return "Blocklist rules";
        return source === "profile" ? "Profile rules" : "User rules";
    }

    // The model's JSON summary of the hit counts; null until the bridge has
    // sent any.
    readonly property var hitsInfo: !!page.model && page.model.hitsInfoJson.length > 0
        ? JSON.parse(page.model.hitsInfoJson) : null

    function formatTime(ms) {
        return new Date(ms).toLocaleString(Qt.locale(), Locale.ShortFormat);
    }

    // Empty when this bridge sends no counts.
    function hitsSummaryText(info) {
        if (!info || !info.available) return "";
        if (!info.counting) {
            return "Hit counts start when the firewall first reports statistics.";
        }
        let text = "Hits counted by Snitchwatch since " + page.formatTime(info.sinceMs)
            + "; approximate.";
        if (info.lossy) {
            text += " Some hits may be missing"
                + (info.lastGapMs > 0 ? " (last noticed " + page.formatTime(info.lastGapMs) + ")" : "")
                + ".";
        }
        return text;
    }

    function hitsStorageText(info) {
        if (!info || !info.available || info.persistent) return "";
        return info.storageReason.length > 0
            ? "Hit counts are not saved across restarts: " + info.storageReason
            : "Hit counts are not saved across restarts.";
    }

    function hitsRowText(counted, count, lastMs, note) {
        if (note.length > 0) return note;
        if (!counted) return "";
        if (count === 0) return "No hits counted";
        return count + (count === 1 ? " hit" : " hits")
            + (lastMs > 0 ? ", last " + page.formatTime(lastMs) : "");
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

    // "Simulate this connection" from the Connections inspector (main.qml routes
    // it here): open the Simulate sheet on the fields the connection carries.
    // `prefillJson` is `ConnectionsModel.simulationPrefillJson`.
    function openSimulator(prefillJson) {
        simulateSheet.prefill(JSON.parse(prefillJson));
        simulateSheet.open();
    }

    // The rule editor (P2.1): "New rule…", or a rule for a connection.
    // `prefillJson` is the simulator's prefill form
    // (`ConnectionsModel.simulationPrefillJson`); empty for a blank rule.
    // main.qml's `openRuleEditor` routes here. Returns whether it opened.
    function openEditor(prefillJson) {
        return prefillJson ? ruleEditor.startPrefill(prefillJson) : ruleEditor.startNew();
    }

    function checkEditable() {
        const json = page.model ? page.model.editableRuleJson(page.inspectName) : "";
        page.inspectNotEditable = json ? JSON.parse(json).notEditable
                                       : "Edit isn't available for this rule.";
    }

    // The inspector's Edit: swap the inspector for the editor.
    function editInspected() {
        if (!page.model) return;
        inspector.close();
        if (!ruleEditor.startEdit(page.model.editableRuleJson(page.inspectName))) {
            page.inspectNotEditable = ruleEditorController.statusText;
            inspector.open();
        }
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
        page.inspectHowItDecides = rule.howItDecides;
        page.inspectSource = rule.source;
        page.inspectBlocklistId = rule.blocklistId;
        page.checkEditable();
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
            objectName: "exportRules"
            text: "Export…"
            icon.name: "document-export"
            enabled: !rulesIo.busy
            onClicked: rulesIo.requestExport()
        }
        Controls.Button {
            objectName: "importRules"
            text: "Import…"
            icon.name: "document-import"
            enabled: !rulesIo.busy && !rulesIo.applying
            onClicked: importDialog.open()
        }
        Controls.Button {
            objectName: "newRule"
            text: "New rule…"
            icon.name: "list-add"
            onClicked: page.openEditor("")
        }
        Controls.Button {
            text: "Simulate"
            icon.name: "system-run"
            onClicked: simulateSheet.open()
        }
    }

    // Issue #44: when some rules apply to every app. Fixed text; the
    // count sits in a PlainText label (InlineMessage can't render data).
    // Below it, what the hit counts are (see the top of this file), then the
    // last import or export outcome.
    header: ColumnLayout {
        visible: page.showsAllAppsNotice || page.showsIoStatus || page.showsEditorStatus
                 || page.showsNotShown
            || hitsSummaryLabel.text.length > 0 || hitsStorageLabel.text.length > 0
        spacing: 0

        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: page.showsAllAppsNotice
            type: Kirigami.MessageType.Information
            text: "Some rules saved by earlier Snitchwatch versions apply to all apps, or to a "
                + "program Snitchwatch can't identify, not only the app that asked. They are "
                + "marked below, each with what deleting it changes."
        }
        // Counts only Snitchwatch's own earlier rules: blocklist or
        // hand-written rules may apply to all apps too, so no "of N".
        Controls.Label {
            objectName: "allAppsCount"
            visible: page.showsAllAppsNotice
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            text: !page.model ? ""
                : page.model.legacyHostOnlyCount === 1
                    ? "1 rule saved by an earlier Snitchwatch version applies to all apps or "
                      + "to an unidentified program"
                    : page.model.legacyHostOnlyCount
                      + " rules saved by earlier Snitchwatch versions apply to all apps or to "
                      + "unidentified programs"
        }
        Controls.Label {
            id: hitsSummaryLabel
            objectName: "hitsSummary"
            visible: text.length > 0
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            text: page.hitsSummaryText(page.hitsInfo)
        }
        Controls.Label {
            id: hitsStorageLabel
            objectName: "hitsStorage"
            visible: text.length > 0
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.neutralTextColor
            text: page.hitsStorageText(page.hitsInfo)
        }
        // The last export or import outcome (P2.7), plain text.
        Controls.Label {
            id: ioStatus
            objectName: "rulesIoStatus"
            visible: page.showsIoStatus
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            text: rulesIo.statusText
            wrapMode: Text.Wrap
        }
        // Rules the list leaves out (issue #61), plain text.
        Controls.Label {
            objectName: "rulesNotShown"
            visible: page.showsNotShown
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: page.model ? page.model.notShownText : ""
        }
        // The rule editor's last result (P2.1), plain text.
        Controls.Label {
            objectName: "ruleEditorStatus"
            visible: page.showsEditorStatus
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            text: ruleEditorController.statusText
            wrapMode: Text.Wrap
        }
    }

    Kirigami.PlaceholderMessage {
        objectName: "rulesEmptyPlaceholder"
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: page.showsEmptyPlaceholder
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
            required property string flagBadge
            required property bool hitsCounted
            required property real hitCount
            required property real lastHitMs
            required property string hitsNote
            required property string howItDecides

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
                    // How often the rule decided a connection; see the header
                    // for what the number means.
                    Controls.Label {
                        objectName: "hitsLabel"
                        visible: text.length > 0
                        textFormat: Text.PlainText
                        text: page.hitsRowText(row.hitsCounted, row.hitCount, row.lastHitMs,
                                               row.hitsNote)
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideRight
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
                    textFormat: Text.PlainText
                    text: row.flagBadge
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
        page.inspectHowItDecides = row.howItDecides;
        page.inspectSource = row.source;
        page.inspectBlocklistId = row.blocklistId;
        page.checkEditable();
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
                // Issue #102: only deny, reject and decide-first rules stop
                // the daemon's check; an allow decides only if none matches.
                Controls.Label {
                    objectName: "inspectHowItDecides"
                    Kirigami.FormData.label: "Precedence"
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    text: "Position " + (page.inspectPrecedence + 1) + " of "
                          + (page.model ? page.model.count : 0) + " in the order the firewall "
                          + "checks rules. " + page.inspectHowItDecides
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

            Controls.Button {
                id: inspectEditButton
                objectName: "inspectEditButton"
                Layout.fillWidth: true
                visible: !page.confirmingDelete
                enabled: page.inspectNotEditable.length === 0
                text: "Edit…"
                icon.name: "document-edit"
                onClicked: page.editInspected()
            }
            // A read-only rule already says why above.
            Controls.Label {
                objectName: "inspectNotEditable"
                visible: page.inspectReadOnlyReason.length === 0 && page.inspectNotEditable.length > 0
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: page.inspectNotEditable
                wrapMode: Text.Wrap
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

    // Rule editor (P2.1): "New rule…", the inspector's Edit, and
    // `openEditor` for a connection.
    RuleEditorController {
        id: ruleEditorController
        Component.onCompleted: startBridgeFeed()
    }
    RuleEditorSheet {
        id: ruleEditor
        controller: ruleEditorController
    }

    // Rule-match simulator sheet, opened by the header's "Simulate" button.
    RuleSimulatorSheet {
        id: simulateSheet
        model: page.model
    }

    // Rule import/export (P2.7).
    RulesIoController {
        id: rulesIo
        Component.onCompleted: startBridgeFeed()
        onExportReady: exportDialog.open()
    }
    // Gives up on an answer that never comes (an older bridge).
    Timer {
        interval: 1000
        repeat: true
        running: rulesIo.busy
        onTriggered: rulesIo.poll()
    }
    FileDialog {
        id: exportDialog
        title: "Export rules"
        fileMode: FileDialog.SaveFile
        defaultSuffix: "json"
        nameFilters: ["Snitchwatch rules (*.json)"]
        onAccepted: rulesIo.writeExport(selectedFile)
        onRejected: rulesIo.exportCancelled()
    }
    FileDialog {
        id: importDialog
        title: "Import rules"
        fileMode: FileDialog.OpenFile
        nameFilters: ["Snitchwatch rules (*.json)"]
        onAccepted: rulesIo.readImport(selectedFile)
    }
    RulesImportSheet {
        id: importSheet
        controller: rulesIo
    }
}
