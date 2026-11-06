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
// re-reads its rule on every model reset (#48) and every result.
//
// Each toggle and delete asks for a result (#152); its note is kept per
// rule (`rules::command_notes`, plain text): inside the inspector of its
// rule, and below the title as "<rule>: <note>" once the inspector closed.
// E.g. a refused turn-off "may come back on", a refused delete's file may
// remain, or a change that may or may not have applied (#120 item 7).
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
// Rule insights (P2.6 Part 2): a zero-count rule gets a badge. "Unused" is
// claimed only when the counts are saved across restarts and the period they
// can be trusted in (since counting began, the rule was created, and the
// bridge's last gap, whichever is latest) is 14 days; otherwise it is
// "No hits since <time>", from the start of that period. "Analyze
// rules" finds rules that can never decide a connection because another one
// matches everything they do and takes precedence ("Never decides"), or only
// "May never decide" when the proof leans on the regular-expression engine. It
// says nothing about what those connections get instead. It checks only
// conditions Snitchwatch can compare, so no finding is not a guarantee.
// Insights describe; nothing here changes, disables or removes a rule.
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
    property CuratedDefaultsModel curatedModel
    readonly property bool inspectManagedDns: inspectName === "snitchwatch-default-dns-resolved" && inspectSource === "recommended"
    DnsConsentDialog { consentModel: page.curatedModel }
    onVisibleChanged: if (!visible && curatedModel) curatedModel.cancelBroadConsent()
    Component.onDestruction: if (curatedModel) curatedModel.cancelBroadConsent()

    // Snapshot of the rule currently shown in the detail sheet.
    property string inspectName: ""
    property string inspectDisplayName: ""
    // Non-empty when Snitchwatch can't edit the rule (the bridge's reason);
    // the rule is still shown because the daemon still enforces it.
    property string inspectReadOnlyReason: ""
    // A rule read-only only for its conditions can still be deleted.
    property bool inspectDeletable: false
    // Whether the Enabled switch works: an editable rule, or a recommended
    // background-service rule, which is otherwise read-only.
    property bool inspectToggleable: false
    property bool inspectEnabled: true
    property string inspectAction: ""
    property string inspectDuration: ""
    property string inspectOperatorSummary: ""
    property int inspectPrecedence: 0
    // How the rule takes part in the daemon's decision (issue #102).
    property string inspectHowItDecides: ""
    property string inspectSource: "user"
    property string inspectSourceLabel: ""
    property string inspectBlocklistId: ""
    property bool confirmingDelete: false
    // Why the rule editor can't change the inspected rule; empty when it can.
    property string inspectNotEditable: ""
    // Exposed for the headless inspector probe (tests/rules_inspector_qml.rs).
    property alias inspectorSheet: inspector
    property alias inspectorEnabledSwitch: inspector.enabledSwitch
    property alias inspectorDeleteButton: inspector.deleteButton
    property alias rulesList: list
    property alias rulesIo: rulesIo
    property alias importSheet: importSheet
    property alias ruleEditor: ruleEditor
    property alias simulatorSheet: simulateSheet
    property alias inspectorEditButton: inspector.editButton
    property alias inspectorCommandNote: inspector.commandNote
    property alias rulesCommandNote: rulesCommandNote
    readonly property bool showsAllAppsNotice: !!page.model && page.model.legacyHostOnlyCount > 0
    // Not `ioStatus.visible`: a child of a hidden header always reads false.
    readonly property bool showsIoStatus: rulesIo.statusText.length > 0 && !importSheet.visible
    // The rule editor's last result once its sheet closed (P2.1).
    readonly property bool showsEditorStatus: ruleEditorController.statusText.length > 0
                                              && !ruleEditor.visible
    // The toggles' and deletes' notes (#152), once the inspector closed.
    readonly property bool showsCommandNote: !!page.model && page.model.commandNotesText.length > 0
                                             && !inspector.visible
    readonly property var commandNotes: !!page.model && page.model.commandNotesJson.length > 0
        ? JSON.parse(page.model.commandNotesJson) : []
    // The inspected rule's note, or "".
    readonly property string inspectCommandNote: {
        const found = page.commandNotes.find(note => note.rule === page.inspectName);
        return found ? found.text : "";
    }
    // What the list leaves out (#61); no "No rules yet" while that shows.
    readonly property bool showsNotShown: !!page.model && page.model.notShownText.length > 0
    // Issue #65: the firewall service's rule count differs from the list's.
    readonly property bool showsCountHint: !!page.model && page.model.countHintText.length > 0
    readonly property bool showsEmptyPlaceholder: (!page.model || page.model.count === 0)
                                                  && !page.showsNotShown && !page.showsCountHint

    // `allow`, `deny`, or an unrecognised action (a neutral colour, N4).
    function actionColor(action) {
        if (action === "allow") return Kirigami.Theme.positiveTextColor;
        return action === "deny" ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.neutralTextColor;
    }

    // The model's JSON summary of the hit counts; null until the bridge has
    // sent any.
    readonly property var hitsInfo: !!page.model && page.model.hitsInfoJson.length > 0
        ? JSON.parse(page.model.hitsInfoJson) : null

    function formatTime(ms) {
        return insightsText.formatTime(ms);
    }

    // The on-demand analysis' state; null before the model has one.
    readonly property var analysisInfo: !!page.model && page.model.analysisJson.length > 0
        ? JSON.parse(page.model.analysisJson) : null

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
        page.inspectToggleable = rule.toggleable;
        page.inspectEnabled = rule.enabled;
        page.inspectAction = rule.action;
        page.inspectDuration = rule.duration;
        page.inspectOperatorSummary = rule.operatorSummary;
        page.inspectPrecedence = rule.precedence;
        page.inspectHowItDecides = rule.howItDecides;
        page.inspectSource = rule.source;
        page.inspectSourceLabel = rule.sourceLabel;
        page.inspectBlocklistId = rule.blocklistId;
        page.checkEditable();
    }

    // The bridge re-sends the whole list after every rule command the daemon
    // answered or didn't, and clears it when its daemon stream goes away
    // (#48); a command it refused before sending gets only a result (#152).
    // Re-read the open rule on both so the sheet never keeps a state the
    // daemon doesn't have; close the sheet if the rule is gone.
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
        if (page.inspectManagedDns && enabled) {
            if (page.curatedModel) page.curatedModel.reenableDns();
            inspector.enabledSwitch.checked = Qt.binding(function () { return page.inspectEnabled; });
            return;
        }
        if (page.inspectManagedDns && !enabled && page.curatedModel)
            page.curatedModel.revokeBroadConsent();
        page.model.setEnabled(page.inspectName, enabled);
        page.inspectEnabled = enabled;
        inspector.enabledSwitch.checked = Qt.binding(function () { return page.inspectEnabled; });
    }

    Connections {
        target: page.model
        function onModelReset() {
            page.refreshInspector();
        }
        function onCommandFinished() {
            page.refreshInspector();
        }
    }

    // "Simulate" panel entry point (Little-Snitch-parity rule-match
    // diagnostics), kept in the header so it stays reachable regardless of
    // scroll position, same rationale as ConnectionsPage's search field.
    titleDelegate: RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        SelectableText {
            text: page.title
            level: 1
            Layout.alignment: Qt.AlignVCenter
            wrapMode: Text.NoWrap
        }
        Item {
            Layout.fillWidth: true
        }
        RoundedButton {
            objectName: "analyzeButton"
            text: "Analyze rules"
            icon.name: "dialog-scripts"
            enabled: !!page.model && !!page.analysisInfo ? page.analysisInfo.state !== "running"
                                                          : !!page.model
            onClicked: page.model.analyze()
        }
        RoundedButton {
            objectName: "exportRules"
            text: "Export…"
            icon.name: "document-export"
            enabled: !rulesIo.busy
            onClicked: rulesIo.requestExport()
        }
        RoundedButton {
            objectName: "importRules"
            text: "Import…"
            icon.name: "document-import"
            enabled: !rulesIo.busy && !rulesIo.applying
            onClicked: importDialog.open()
        }
        RoundedButton {
            objectName: "newRule"
            text: "New rule…"
            icon.name: "list-add"
            onClicked: page.openEditor("")
        }
        RoundedButton {
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
                 || page.showsCommandNote
                 || page.showsNotShown || page.showsCountHint
            || hitsSummaryLabel.text.length > 0 || hitsStorageLabel.text.length > 0
            || analysisLabel.text.length > 0
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
        SelectableText {
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
            wrapMode: Text.NoWrap
        }
        SelectableText {
            id: hitsSummaryLabel
            objectName: "hitsSummary"
            visible: text.length > 0
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            text: insightsText.hitsSummaryText(page.hitsInfo)
        }
        SelectableText {
            id: hitsStorageLabel
            objectName: "hitsStorage"
            visible: text.length > 0
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.neutralTextColor
            text: insightsText.hitsStorageText(page.hitsInfo)
        }
        SelectableText {
            id: analysisLabel
            objectName: "analysisSummary"
            visible: text.length > 0
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            text: insightsText.analysisText(page.analysisInfo)
        }
        // The last export or import outcome (P2.7), plain text.
        SelectableText {
            id: ioStatus
            objectName: "rulesIoStatus"
            visible: page.showsIoStatus
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            text: rulesIo.statusText
            wrapMode: Text.Wrap
        }
        SelectableText {
            objectName: "rulesNotShown"
            visible: page.showsNotShown
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: page.model ? page.model.notShownText : ""
        }
        // Issue #65: fixed text, advice only (the list is not re-sent).
        SelectableText {
            objectName: "rulesCountHint"
            visible: page.showsCountHint
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.neutralTextColor
            text: page.model ? page.model.countHintText : ""
        }
        // Each rule's last toggle or delete note (#152), plain text.
        SelectableText {
            id: rulesCommandNote
            objectName: "rulesCommandNote"
            visible: page.showsCommandNote
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            text: page.model ? page.model.commandNotesText : ""
            wrapMode: Text.Wrap
        }
        // The rule editor's last result (P2.1), plain text.
        SelectableText {
            objectName: "ruleEditorStatus"
            visible: page.showsEditorStatus
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            textFormat: Text.PlainText
            text: ruleEditorController.statusText
            wrapMode: Text.Wrap
        }
    }

    ListView {
        id: list
        model: page.model
        currentIndex: -1
        reuseItems: true
        // Q01: Tab lands on a rule (its row takes focus); select it then, so
        // arrows move from it. Space opens a rule (ItemDelegate), and so do
        // Enter and Return.
        onActiveFocusChanged: if (activeFocus && currentIndex < 0 && count > 0) currentIndex = 0
        // The rule that has keyboard focus (a row's own focus, however Tab
        // got there), read at key time; the selected row only when focus is
        // on the list itself. `openInspector` copies the rule's fields at
        // once, so a later reorder cannot change which rule opens.
        function openCurrent() {
            let target = null;
            for (let it = Window.activeFocusItem; it && !target; it = it.parent) {
                if (it.name !== undefined && it.displayName !== undefined && it.ruleAction !== undefined) {
                    target = it;
                }
            }
            target = target || list.currentItem;
            if (target) {
                page.openInspector(target);
            }
        }
        Keys.onReturnPressed: list.openCurrent()
        Keys.onEnterPressed: list.openCurrent()
        // The Menu key opens the rule's actions (its inspector: turn on or
        // off, edit, delete), as it opens a connection's menu.
        Keys.onMenuPressed: list.openCurrent()

        RulesEmptyPlaceholder {
            parent: list
            visible: page.showsEmptyPlaceholder
            listed: !!page.model && page.model.listed
        }

        section.property: "sectionLabel"
        section.criteria: ViewSection.FullString
        section.delegate: Kirigami.ListSectionHeader {
            width: ListView.view ? ListView.view.width : implicitWidth
            text: section
        }

        delegate: Controls.ItemDelegate {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            highlighted: ListView.isCurrentItem
            // Focus, highlight and the arrows' start point always agree.
            onActiveFocusChanged: if (activeFocus) list.currentIndex = row.index
            opacity: row.source === "blocklist" ? 0.7 : 1.0

            required property int index
            required property string name
            required property string displayName
            required property string readOnlyReason
            required property bool deletable
            required property bool toggleable
            // Keep the rule flag separate from Item.enabled so disabled rules
            // remain interactive and can be enabled again in the inspector.
            required property var model
            readonly property bool ruleEnabled: model.enabled
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
            required property string sourceLabel
            required property string hitBadgeKind
            required property real hitBadgeMs
            required property string shadowKind
            required property string shadowText
            required property string shadowBy

            onClicked: {
                list.currentIndex = row.index;
                page.openInspector(row);
            }

            // Every text the row shows, in the order shown (one per line);
            // the row's Labels stay Labels so a click still opens the rule.
            function copyLines() {
                const lines = [row.ruleEnabled ? "●" : "○"];
                lines.push(row.source === "blocklist" ? ("blocklist: " + row.blocklistId) : row.displayName);
                if (row.source !== "blocklist" && row.operatorSummary.length > 0) {
                    lines.push(row.operatorSummary);
                }
                const hits = insightsText.hitsRowText(row.hitsCounted, row.hitCount, row.lastHitMs,
                                                      row.hitsNote, row.hitBadgeKind, row.hitBadgeMs);
                if (hits.length > 0) {
                    lines.push(hits);
                }
                if (row.shadowText.length > 0) {
                    lines.push(row.shadowText);
                }
                if (row.appliesToAllApps) {
                    lines.push(row.allAppsHint, row.flagBadge);
                }
                if (row.readOnlyReason.length > 0) {
                    lines.push("read-only");
                }
                lines.push("#" + row.precedence, row.ruleAction);
                return lines.filter(line => line.length > 0);
            }

            // Right-click Copy of the row's text (mouse only; the Menu key still
            // opens the inspector, whose Copy buttons are the keyboard route).
            RowCopyMenu {
                copyText: row.copyLines().join("\n")
            }

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                Controls.Label {
                    text: row.ruleEnabled ? "●" : "○"
                    color: row.ruleEnabled ? page.actionColor(row.ruleAction) : Kirigami.Theme.disabledTextColor
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
                        text: insightsText.hitsRowText(row.hitsCounted, row.hitCount, row.lastHitMs,
                                               row.hitsNote, row.hitBadgeKind, row.hitBadgeMs)
                        opacity: row.hitBadgeKind === "unused" ? 1.0 : 0.7
                        color: row.hitBadgeKind === "unused" ? Kirigami.Theme.neutralTextColor
                                                              : Kirigami.Theme.textColor
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    // A finding from "Analyze rules": this rule never (or may
                    // never) decides a connection, because the named rule
                    // matches all of them and takes precedence. The link
                    // opens that rule.
                    RowLayout {
                        visible: row.shadowText.length > 0
                        Layout.fillWidth: true
                        Controls.Label {
                            objectName: "shadowLabel"
                            textFormat: Text.PlainText
                            text: row.shadowText
                            wrapMode: Text.Wrap
                            font: Kirigami.Theme.smallFont
                            color: Kirigami.Theme.neutralTextColor
                            Layout.fillWidth: true
                        }
                        RoundedButton {
                            objectName: "shadowShow"
                            flat: true
                            text: "Show rule"
                            font: Kirigami.Theme.smallFont
                            onClicked: page.openRuleByName(row.shadowBy)
                        }
                    }
                    // Issue #44: what deleting this all-apps rule changes.
                    Controls.Label {
                        objectName: "allAppsHint"
                        visible: row.appliesToAllApps
                        textFormat: Text.PlainText
                        text: row.allAppsHint
                        wrapMode: Text.Wrap
                        font: Kirigami.Theme.smallFont
                        color: row.ruleAction === "deny" ? Kirigami.Theme.negativeTextColor
                                                         : Kirigami.Theme.neutralTextColor
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
                RoundedButton {
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
        page.inspectToggleable = row.toggleable;
        page.inspectEnabled = row.ruleEnabled;
        page.inspectAction = row.ruleAction;
        page.inspectDuration = row.duration;
        page.inspectOperatorSummary = row.operatorSummary;
        page.inspectPrecedence = row.precedence;
        page.inspectHowItDecides = row.howItDecides;
        page.inspectSource = row.source;
        page.inspectSourceLabel = row.sourceLabel;
        page.inspectBlocklistId = row.blocklistId;
        page.checkEditable();
        page.confirmingDelete = false;
        inspector.open();
    }

    // The rule detail sheet (enable/disable, Edit, delete).
    RulesInspectorSheet {
        id: inspector
        host: page
    }

    // Rule editor (P2.1): "New rule…", the inspector's Edit, and
    // `openEditor` for a connection.
    RulesInsightsText {
        id: insightsText
        unusedWindowMs: page.hitsInfo ? page.hitsInfo.unusedWindowMs : 0
    }

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
