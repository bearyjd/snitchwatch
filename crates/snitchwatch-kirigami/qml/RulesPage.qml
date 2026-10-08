// Rules tab (Task 10 — view layer over RulesModel).
//
// Same list/detail shape as BlocklistsPage.qml (Task 9): a ListView bound to
// `RulesModel`, with a Kirigami.OverlaySheet detail view for enable/disable +
// delete + precedence display.
//
// Grouping: per the design doc's "blocklist verdict type" section, every
// subscribed blocklist entry is materialized into a deny rule in the
// `z00-blocklist:<id>:` filename band. Those rules are the SAME underlying
// deny rules already shown in full (per-host) on the Blocklists tab, so this
// page groups them into their own "Blocklist rules" section — via
// ListView.section keyed on the model's `source` role — rendered visually
// muted (reduced opacity, no operator-summary line) rather than repeating
// per-host detail that would just confuse the two tabs' purposes. User rules
// always evaluate first regardless (see the design doc's specificity
// section), independent of this display grouping.
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

    // Simulate panel state (rule-match diagnostics' "Simulate" sheet — see
    // `rules::simulator` module docs for exactly what semantics are
    // reproduced). `simulateRan` distinguishes "never run" from "ran, no
    // match" so the result section only appears after a real attempt.
    property bool simulateRan: false
    property string simulateMatchedRule: ""
    property string simulateAction: ""
    property int simulatePrecedence: -1
    // The three below are plain-text lines built from the simulator's result
    // (rule names are data, so they only ever go into PlainText labels).
    // Operands that can't be simulated, with the reason.
    property string simulateUnsupported: ""
    // Conditions left undecided because an advanced input was blank.
    property string simulateUnevaluated: ""
    // Notes on how the deciding rule matched (hash conditions).
    property string simulateWarnings: ""
    // Exposed for the headless simulator probe
    // (tests/rules_simulator_advanced_qml.rs).
    property alias simulateUidField: simUid
    property alias simulateChecksumsBox: simChecksums

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

    // Run the rule-match simulator (Qt-free logic in `rules::simulator`)
    // against the sheet's candidate inputs and populate the result section.
    function runSimulation() {
        if (!page.model) return;
        const json = page.model.simulate(JSON.stringify({
            processPath: simProcessPath.text,
            destHost: simHost.text,
            destPort: simPort.value,
            protocol: simProtocol.currentText,
            parentPaths: simParentPaths.text,
            command: simCommand.text,
            pid: simPid.text,
            uid: simUid.text,
            env: simEnv.text,
            srcIp: simSrcIp.text,
            srcPort: simSrcPort.text,
            destIp: simDestIp.text,
            ifaceIn: simIfaceIn.text,
            ifaceOut: simIfaceOut.text,
            checksums: simChecksums.modes[simChecksums.currentIndex],
            md5: simMd5.text
        }));
        if (!json) return;
        const result = JSON.parse(json);
        page.simulateMatchedRule = result.matchedRule || "";
        page.simulateAction = result.action || "";
        page.simulatePrecedence = (result.precedence === undefined || result.precedence === null)
            ? -1 : result.precedence;
        page.simulateUnsupported = (result.unsupportedOperands || [])
            .map(function (u) { return u.operand + " — " + u.reason; })
            .join("\n");
        const undecided = result.unevaluated || [];
        const shown = undecided.slice(0, 10).map(function (u) {
            return u.rule + ": " + u.operand + " — input missing: " + u.missing;
        });
        if (undecided.length > shown.length) {
            shown.push("and " + (undecided.length - shown.length) + " more");
        }
        page.simulateUnevaluated = shown.join("\n");
        page.simulateWarnings = (result.warnings || []).join("\n");
        page.simulateRan = true;
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

    // Rule-match simulator (Little-Snitch-parity "Simulate" panel). Pure,
    // synchronous evaluation over already-cached rules (`RulesModel.simulate`
    // -> `rules::simulator::simulate`) — never touches the network, so this
    // is safe to run directly from the UI thread. Every result is labelled a
    // simulation, never a live daemon verdict (see `rules::simulator` module
    // docs for exactly what operand types are and aren't reproduced).
    SizedOverlaySheet {
        id: simulateSheet
        title: "Simulate rule match"
        preferredWidth: Kirigami.Units.gridUnit * 22

        ColumnLayout {
            Layout.preferredWidth: simulateSheet.preferredWidth
            spacing: Kirigami.Units.largeSpacing

            Controls.Label {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                text: "Evaluates a candidate connection against the currently cached rules, using opensnitchd's own precedence rules. This is a simulation over cached data, not a live daemon verdict. A blank field is unknown, so rules with a condition on it are reported as not evaluated instead of being guessed; the one exception is the destination host, where blank means a connection to a bare IP address."
            }

            Kirigami.FormLayout {
                Layout.fillWidth: true

                Controls.TextField {
                    id: simProcessPath
                    Kirigami.FormData.label: "Process path"
                    placeholderText: "/usr/bin/curl"
                    Layout.fillWidth: true
                }
                Controls.TextField {
                    id: simHost
                    Kirigami.FormData.label: "Destination host"
                    placeholderText: "github.com"
                    Layout.fillWidth: true
                }
                Controls.SpinBox {
                    id: simPort
                    Kirigami.FormData.label: "Destination port"
                    from: 0
                    to: 65535
                    value: 443
                }
                Controls.ComboBox {
                    id: simProtocol
                    Kirigami.FormData.label: "Protocol"
                    // opensnitchd names IPv6 flows tcp6/udp6/...; a rule for
                    // `tcp` does not match `tcp6`.
                    model: ["tcp", "tcp6", "udp", "udp6", "udplite", "udplite6",
                            "sctp", "sctp6", "icmp", "icmp6"]
                }
            }

            Controls.Button {
                Layout.fillWidth: true
                text: advancedInputs.visible ? "Hide advanced inputs" : "Advanced inputs"
                icon.name: advancedInputs.visible ? "arrow-up" : "arrow-down"
                onClicked: advancedInputs.visible = !advancedInputs.visible
            }

            // Everything opensnitchd can match on beyond the four fields
            // above. Every field here is optional: blank means UNKNOWN (the
            // simulator reports rules that need it as not evaluated, it never
            // guesses). `runSimulation` sends them as typed.
            ColumnLayout {
                id: advancedInputs
                Layout.fillWidth: true
                visible: false
                spacing: Kirigami.Units.largeSpacing

                Controls.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    opacity: 0.7
                    font: Kirigami.Theme.smallFont
                    text: "Everything else opensnitchd can match on. Leave a field blank if you don't know it."
                }

                Kirigami.FormLayout {
                    Layout.fillWidth: true

                    Controls.TextArea {
                        id: simParentPaths
                        Kirigami.FormData.label: "Parent programs"
                        placeholderText: "One path per line, nearest first"
                        Layout.fillWidth: true
                        Layout.preferredHeight: Kirigami.Units.gridUnit * 4
                    }
                    Controls.TextField {
                        id: simCommand
                        Kirigami.FormData.label: "Command line"
                        placeholderText: "/usr/bin/curl -s https://github.com"
                        Layout.fillWidth: true
                    }
                    Controls.TextField {
                        id: simPid
                        Kirigami.FormData.label: "Process ID"
                        validator: IntValidator { bottom: 0; top: 2147483647 }
                        Layout.fillWidth: true
                    }
                    Controls.TextField {
                        id: simUid
                        Kirigami.FormData.label: "User ID"
                        placeholderText: "1000"
                        validator: IntValidator { bottom: 0; top: 2147483647 }
                        Layout.fillWidth: true
                    }
                    Controls.TextArea {
                        id: simEnv
                        Kirigami.FormData.label: "Environment"
                        placeholderText: "NAME=value, one per line; variables not listed count as unset"
                        Layout.fillWidth: true
                        Layout.preferredHeight: Kirigami.Units.gridUnit * 4
                    }
                    Controls.TextField {
                        id: simSrcIp
                        Kirigami.FormData.label: "Source IP"
                        placeholderText: "192.168.1.10"
                        Layout.fillWidth: true
                    }
                    Controls.TextField {
                        id: simSrcPort
                        Kirigami.FormData.label: "Source port"
                        validator: IntValidator { bottom: 0; top: 65535 }
                        Layout.fillWidth: true
                    }
                    Controls.TextField {
                        id: simDestIp
                        Kirigami.FormData.label: "Destination IP"
                        placeholderText: "93.184.216.34"
                        Layout.fillWidth: true
                    }
                    Controls.TextField {
                        id: simIfaceIn
                        Kirigami.FormData.label: "Inbound interface"
                        placeholderText: "eth0"
                        Layout.fillWidth: true
                    }
                    Controls.TextField {
                        id: simIfaceOut
                        Kirigami.FormData.label: "Outbound interface"
                        placeholderText: "eth0"
                        Layout.fillWidth: true
                    }
                    Controls.ComboBox {
                        id: simChecksums
                        Kirigami.FormData.label: "Checksums"
                        // Parallel to `modes`, which is what the simulator reads.
                        readonly property var modes: ["unknown", "off", "on", "on-none"]
                        model: ["Unknown", "Off", "On, program's MD5 below",
                                "On, program has none recorded"]
                    }
                    Controls.TextField {
                        id: simMd5
                        Kirigami.FormData.label: "Program MD5"
                        placeholderText: "Leave blank if unknown"
                        enabled: simChecksums.currentIndex === 2
                        Layout.fillWidth: true
                    }
                }
            }

            Controls.Button {
                Layout.fillWidth: true
                text: "Run simulation"
                icon.name: "system-run"
                onClicked: page.runSimulation()
            }

            Kirigami.Separator {
                Layout.fillWidth: true
                visible: page.simulateRan
            }

            ColumnLayout {
                Layout.fillWidth: true
                visible: page.simulateRan
                spacing: Kirigami.Units.smallSpacing

                Controls.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    opacity: 0.7
                    font: Kirigami.Theme.smallFont
                    text: "Simulated result: not a live daemon verdict."
                }
                Controls.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    font.bold: true
                    textFormat: Text.PlainText
                    text: page.simulateMatchedRule.length > 0
                          ? ("Matched: " + page.simulateMatchedRule)
                          : "No match — the daemon's default action would apply"
                    color: page.simulateMatchedRule.length > 0
                           ? page.actionColor(page.simulateAction)
                           : Kirigami.Theme.neutralTextColor
                }
                Controls.Label {
                    visible: page.simulateMatchedRule.length > 0
                    textFormat: Text.PlainText
                    text: "Action: " + page.simulateAction + "  ·  Position " + (page.simulatePrecedence + 1)
                    color: page.actionColor(page.simulateAction)
                }
                Controls.Label {
                    Layout.fillWidth: true
                    visible: page.simulateWarnings.length > 0
                    wrapMode: Text.Wrap
                    opacity: 0.8
                    font: Kirigami.Theme.smallFont
                    color: Kirigami.Theme.neutralTextColor
                    textFormat: Text.PlainText
                    text: page.simulateWarnings
                }
                Controls.Label {
                    Layout.fillWidth: true
                    visible: page.simulateUnevaluated.length > 0
                    wrapMode: Text.Wrap
                    opacity: 0.8
                    font: Kirigami.Theme.smallFont
                    color: Kirigami.Theme.neutralTextColor
                    textFormat: Text.PlainText
                    text: "Not evaluated, because an input was left blank. The result assumes these rules did not match:\n"
                          + page.simulateUnevaluated
                }
                Controls.Label {
                    Layout.fillWidth: true
                    visible: page.simulateUnsupported.length > 0
                    wrapMode: Text.Wrap
                    opacity: 0.8
                    font: Kirigami.Theme.smallFont
                    color: Kirigami.Theme.neutralTextColor
                    textFormat: Text.PlainText
                    text: "Can't simulate these conditions. The result assumes the rules using them did not match:\n"
                          + page.simulateUnsupported
                }
            }
        }
    }
}
