// Connections list page (Task 8 — view layer + filter/search/inspector).
//
// Binds a ListView to the Rust `ConnectionsModel` (Task 6). Row delegate shows
// process / host:port / protocol with a verdict marker: a hollow ◐ for pending
// rows, allow-green / deny-red for decided ones (per the design spec's
// pending-row styling).
//
// This page adds the Task 8 remainder:
//   * Search/filter — the header SearchField and "pending only" toggle drive
//     `ConnectionsModel.setFilterQuery` / `setPendingOnly`. All filtering logic
//     lives in Rust (`connections::filter`); QML only forwards user input.
//   * Auto-select on new pending row — the model emits `autoSelectRequested`
//     with a visible index; we move `currentIndex` there but do NOT pop the
//     inspector, so it never steals interaction focus from the user.
//   * Inspector — a Kirigami.OverlaySheet opened by clicking a row (design
//     decision: OverlaySheet over a SplitView detail column, chosen because it
//     works identically on narrow and wide windows and matches the pending-row
//     inspector described in the design spec without a responsive-layout branch).
//
// Little-Snitch-parity grouping (Process -> Domain -> connection): the
// header's "Grouped" switch drives `ConnectionsModel.setGrouped`. In grouped
// mode the same `ListView`/delegate renders a mix of row kinds — process
// headers (depth 0), domain headers (depth 1), and leaf connection rows
// (depth 2) — distinguished by the `isGroupHeader`/`depth` roles the Rust
// model exposes; clicking a header toggles its expand state via
// `toggleProcessGroup`/`toggleDomainGroup` instead of opening the inspector.
// All grouping/aggregate logic lives in Rust (`connections::grouping`); this
// QML only renders the flattened projection and forwards toggle clicks.
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

    // Live-wiring hub (Task 13), injected by main.qml. Threaded down to the
    // embedded PendingDecisionSheet so a submitted verdict reaches the bridge's
    // inbound pump. Null in isolated component tests (the sheet no-ops then).
    property var bridgeFeed: null

    // The inline row and process-header buttons: what they send and what
    // they say (plan 2026-10-08-inline-deny-until-restart.md).
    InlineVerdicts {
        id: verdictHelper
        model: page.model
        bridgeFeed: page.bridgeFeed
        onExplained: text => page.showPassiveNotice(text)
    }
    // Exposed for the headless probe (tests/inline_verdict_qml.rs).
    property alias inlineVerdicts: verdictHelper

    // Issue #18: a row's inline Allow/Deny. Also the entry point
    // `tests/inline_verdict_qml.rs` drives, to exercise the click -> submit ->
    // verdict-message path without synthesizing a real mouse click.
    function submitInlineVerdict(rowId, choice) {
        verdictHelper.submit(rowId, choice);
    }

    // Issue #18: a process header's "Allow all"/"Deny all".
    function submitBatchVerdict(processKey, choice, sourceSession) {
        verdictHelper.submitBatch(processKey, choice, sourceSession);
    }

    // Snapshot of the row currently shown in the inspector sheet.
    property string inspectId: ""
    property string inspectProcess: ""
    property string inspectHost: ""
    property int inspectPort: 0
    property string inspectProtocol: ""
    property string inspectVerdict: ""
    property bool inspectPending: false
    // Issue #49: set when the row the inspector was opened on as pending has
    // since gone away (see `recheckInspectedRow`). Shown as a notice in place of
    // the decision sheet.
    property bool inspectNoLongerPending: false
    // What the inspector's Verdict row says. `inspectVerdict` is the copy taken
    // when it opened, so for a withdrawn prompt it would still read "pending".
    readonly property string inspectVerdictText: page.inspectNoLongerPending
        ? "no longer pending"
        : (page.inspectPending ? "pending"
           : page.inspectOutcomeText !== "" ? page.inspectOutcomeText : page.inspectVerdict)
    // Prompt-slot plan Part C, copied from the row like the rest: when the
    // bridge answers it (-1: never), a put-off row's label, whether "Make a
    // rule…" is offered (put off, or decided by the firewall's default
    // action, E3), and whether its session takes "Decide later".
    property real inspectDeadlineMs: -1
    property string inspectOutcomeText: ""
    property bool inspectMakeRuleOffered: false
    property bool inspectDecideLater: false
    // Parity 2 (pending-decision insight panel) — pulled from
    // `ConnectionsModel.rowDetailsJson` alongside the rest of the inspector
    // snapshot. Per-connection byte counters are deliberately not surfaced:
    // the bridge reports them as a hardcoded 0 (issue #49).
    property string inspectIp: ""
    // Issue #44: whether an answer for this row's program can be remembered
    // (`rowDetailsJson`'s `bindableProcessPath`); false until known.
    property bool inspectBindableProcessPath: false
    // E3: whether the daemon may list the inspected put-off row's connection
    // again as decided by its default action (`rowDetailsJson`'s
    // `alsoListedByDefault`); false until known.
    property bool inspectAlsoListedByDefault: false
    // Whether the inspected row's bridge advertised app-bound rules
    // (`InlineVerdicts.rowAppBoundRules`); false until known.
    property bool inspectAppBoundRules: false
    // Exposed for the headless probes (tests/verdict_not_remembered_qml.rs,
    // tests/inline_verdict_qml.rs).
    property alias decisionSheet: pendingSheet
    property alias makeRuleSheet: makeRuleSheet
    property alias alsoListedNote: alsoListedNote
    property alias connectionList: list
    // Raw matched-rule name (empty when unknown/not applicable — drives the
    // "Show rule" button's visibility) and its friendly display string (never
    // blank — see `connections::row_store::matched_rule_display`).
    property string inspectMatchedRule: ""
    property string inspectMatchedRuleDisplay: ""

    // Rule-match diagnostics (Little-Snitch-parity "which rule decided this
    // connection" — jump to the Rules tab). Emitted by the inspector's "Show
    // rule" button; main.qml routes it to a RulesPage navigation + highlight,
    // since this page has no direct reference to the Rules tab's model/page.
    signal showRuleRequested(string ruleName)

    // "Simulate this connection": the inspector's button asks the model for the
    // row's known fields (`ConnectionsModel.simulationPrefillJson`; anything
    // the row doesn't carry stays blank, which the simulator reads as
    // unknown) and emits them. main.qml routes this to the Rules tab's
    // Simulate sheet, which this page has no reference to.
    signal simulateConnectionRequested(string prefillJson)

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

    // React to the Rust auto-select policy: move the selection, but never open
    // the inspector — surfacing a pending row must not grab the user's focus.
    Connections {
        target: page.model
        function onAutoSelectRequested(row) {
            list.currentIndex = row;
        }
        // Issue #49: the bridge removes a pending row when opensnitchd's ask
        // times out, a snapshot starts with ClearConnectionRows and then
        // re-inserts what is still pending, and a verdict decided elsewhere
        // arrives as an update. Flat mode reports these as rowsRemoved,
        // rowsInserted and dataChanged; every other mutation (clear, grouped
        // mode, an active filter) brackets a full reset.
        function onRowsRemoved() {
            page.recheckInspectedRow();
        }
        function onRowsInserted() {
            page.recheckInspectedRow();
        }
        function onDataChanged() {
            page.recheckInspectedRow();
        }
        function onModelReset() {
            page.recheckInspectedRow();
        }
    }

    // Issue #49: losing the bridge connection ends every prompt it held, but
    // the model keeps the lost session's pending rows until the next
    // snapshot replaces them. Only losing the connection acts here: regaining
    // it must not bring a prompt back, because the old session's ids never
    // return (the snapshot decides). The stub feed in some component tests has
    // no `ok` property.
    Connections {
        target: page.bridgeFeed
        ignoreUnknownSignals: true
        function onOkChanged() {
            if (page.bridgeFeed.ok === false) {
                page.recheckInspectedRow();
            }
        }
        function onVerdictNotRemembered(rowId) {
            page.showVerdictNotRemembered();
        }
    }

    // Issue #44: the bridge answered a remembered verdict for this connection
    // only. Fixed text, never the wire `reason`.
    function showVerdictNotRemembered() {
        page.showPassiveNotice(verdictHelper.notRememberedSentence);
    }

    function showPassiveNotice(text) {
        const win = Controls.ApplicationWindow.window;
        if (win && typeof win.showPassiveNotification === "function") {
            win.showPassiveNotification(text, "long");
        }
    }

    // Search + pending-only filter live in the page header so they stay visible
    // while the list scrolls.
    titleDelegate: RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Heading {
            text: page.title
            level: 1
            Layout.alignment: Qt.AlignVCenter
        }
        Kirigami.SearchField {
            id: search
            Layout.fillWidth: true
            placeholderText: "Filter by process, host, port…"
            onTextChanged: page.model.setFilterQuery(text)
        }
        Controls.Switch {
            id: pendingOnly
            text: "Pending only"
            onToggled: page.model.setPendingOnly(checked)
        }
        Controls.Switch {
            id: grouped
            text: "Grouped"
            checked: page.model ? page.model.grouped : true
            // Must be the invokable, not the auto-generated `setGrouped`
            // property setter: only setGroupedMode brackets the Qt model
            // reset and rebuilds the grouped projection.
            onToggled: page.model.setGroupedMode(checked)
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: (page.model && page.model.totalCount > 0) ? "search" : "network-connect"
        text: (page.model && page.model.totalCount > 0)
              ? "No matching connections"
              : "No connections yet"
        explanation: (page.model && page.model.totalCount > 0)
              ? "No rows match the current filter."
              : "New connection prompts and recent decisions will appear here."
    }

    ListView {
        id: list
        model: page.model
        currentIndex: -1
        reuseItems: true

        // Keep the Rust model's notion of the current selection in sync so its
        // auto-select policy knows whether the user is investigating a row.
        onCurrentIndexChanged: {
            const item = list.itemAtIndex(list.currentIndex);
            page.model.setCurrentRowId(item ? item.rowId : "");
        }

        delegate: Item {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            implicitHeight: content.implicitHeight + Kirigami.Units.smallSpacing * 2

            required property int index
            required property string rowId
            required property string sourceSession
            required property string process
            required property string host
            required property int port
            required property string protocol
            required property string verdict
            required property bool pending

            // Grouping roles (Little-Snitch-parity Process->Domain view).
            // depth: 0 = process header, 1 = domain header, 2 = leaf
            // connection row. Flat mode always reports depth 0 / isGroupHeader
            // false, so this delegate renders identically to before grouping
            // was added.
            required property int depth
            required property bool isGroupHeader
            required property bool expanded
            required property string groupKey
            required property string groupParentKey
            required property string groupLabel
            required property int groupTotal
            required property int groupPending
            required property int groupAllowed
            required property int groupDenied
            required property int groupBlocklisted

            // Rule-match diagnostics roles. Header rows report empty strings
            // for both (see `ConnectionsModel::grouped_entry_data`) since
            // they aren't a single decided connection.
            required property string matchedRule
            required property string matchedRuleDisplay
            // The bridge allowed this row once because filtering was paused
            // (issue #78). Always false on headers.
            required property bool answeredWhilePaused
            // Prompt-slot plan Part C: a put-off or default-decided row's
            // label (empty otherwise), when the bridge answers a pending one
            // (-1: never), and whether "Make a rule…" is offered (put off,
            // or decided by the firewall's default action).
            required property string outcomeText
            required property real answerDeadlineMs
            required property bool makeRuleOffered

            // Issue #18 double-submit guard: the inline/batch buttons stay
            // visible until the round trip flips `pending` to false, so a
            // second click before that arrives would otherwise send a
            // second (safe but ERROR-logged, feedback-less) SetVerdict.
            // Reset whenever this delegate instance starts representing a
            // different row (reuseItems recycles delegates, so `rowId`
            // changing is the reliable "this is a new row now" signal) or
            // when the row's own pending state changes (a fresh pending
            // arrival on the same id, or a decision landing).
            property bool submitted: false
            onRowIdChanged: row.submitted = false
            onPendingChanged: row.submitted = false

            // What this row's Deny, or this header's "Deny all", does: the
            // buttons' tooltip and accessible description. Empty where the
            // button is hidden, so other delegates never look it up.
            readonly property string denyText: !row.isGroupHeader && row.pending
                ? verdictHelper.denyText(row.rowId) : ""
            readonly property string denyAllText: row.isGroupHeader && row.depth === 0
                && row.groupPending > 0 ? verdictHelper.denyAllText(row.groupKey) : ""
            // Exposed for the headless probe (tests/inline_verdict_qml.rs).
            property alias denyButton: rowDenyButton
            property alias denyAllButton: batchDenyButton

            // Single latch-and-dispatch for all 4 verdict buttons. Kept as one
            // function so the re-entry guard can't be present on some sites
            // and missing on others — it already was once, leaving "Allow
            // all" able to double-submit every pending row under a process.
            // `batch` picks the process-group action over the single-row one.
            //
            // These buttons used to ALSO carry a `TapHandler`, added on the
            // theory that a nested action needs its own pointer grab. Measured
            // with QtTest `mouseClick()` against this exact delegate shape
            // (Item > MouseArea z:0 + content z:1 > Button): one real click
            // fires `onClicked` AND `onTapped` — 1 each, identical across the
            // Basic/Fusion/Material/Universal styles — while the underlying
            // MouseArea correctly stays silent. So the TapHandler was pure
            // double-dispatch and is gone; plain `onClicked` is sufficient.
            readonly property bool decideLaterOffered: !row.isGroupHeader && row.pending
                && verdictHelper.rowDecideLater(row.rowId)

            function decideOnce(choice, batch) {
                if (row.submitted) {
                    return;
                }
                row.submitted = true;
                if (batch) {
                    page.submitBatchVerdict(row.groupKey, choice, row.sourceSession);
                } else if (choice === "later") {
                    // Part C "Decide later": not a verdict QML builds.
                    verdictHelper.decideLater(row.rowId);
                } else {
                    page.submitInlineVerdict(row.rowId, choice);
                }
            }

            // Deliberately behind `content`: leaf/header row clicks work in
            // the remaining space, while nested Buttons receive their own
            // pointer events instead of being swallowed by ItemDelegate.
            MouseArea {
                anchors.fill: parent
                z: 0
                onClicked: {
                    if (row.isGroupHeader) {
                        if (row.depth === 0) {
                            page.model.toggleProcessGroup(row.groupKey);
                        } else {
                            page.model.toggleDomainGroup(row.groupParentKey, row.groupKey);
                        }
                        return;
                    }
                    list.currentIndex = row.index;
                    page.openInspector(row);
                }
            }

            RowLayout {
                id: content
                anchors.fill: parent
                anchors.margins: Kirigami.Units.smallSpacing
                z: 1
                spacing: Kirigami.Units.largeSpacing

                Item {
                    // Indent nested rows/headers under their ancestors.
                    Layout.preferredWidth: row.depth * Kirigami.Units.gridUnit
                }

                Kirigami.Icon {
                    visible: row.isGroupHeader
                    source: row.expanded ? "arrow-down" : "arrow-right"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.small
                    Layout.preferredHeight: Kirigami.Units.iconSizes.small
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    visible: !row.isGroupHeader
                    textFormat: Text.PlainText
                    text: page.verdictGlyph(row.verdict, row.pending)
                    color: page.verdictColor(row.verdict)
                    Layout.alignment: Qt.AlignVCenter
                }

                ColumnLayout {
                    visible: !row.isGroupHeader
                    Layout.fillWidth: true
                    spacing: 0
                    Controls.Label {
                        textFormat: Text.PlainText
                        text: row.process
                        font.bold: row.pending
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        textFormat: Text.PlainText
                        text: row.host + ":" + row.port + "  " + row.protocol
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                }

                Controls.Label {
                    objectName: "verdictLabel"
                    visible: !row.isGroupHeader
                    textFormat: Text.PlainText
                    text: row.pending ? "pending"
                        : row.outcomeText !== "" ? row.outcomeText
                        : row.answeredWhilePaused ? "Allowed once (filtering was paused)"
                        : row.verdict
                    color: page.verdictColor(row.verdict)
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    visible: row.isGroupHeader
                    textFormat: Text.PlainText
                    text: row.groupLabel
                    font.bold: true
                    color: row.groupPending > 0 ? Kirigami.Theme.neutralTextColor : Kirigami.Theme.textColor
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

                Controls.Label {
                    visible: row.isGroupHeader
                    text: row.groupTotal + " connection" + (row.groupTotal === 1 ? "" : "s")
                    color: Kirigami.Theme.disabledTextColor
                    Layout.alignment: Qt.AlignVCenter
                }

                Rectangle {
                    // Pending-count badge. Groups are never force-expanded
                    // for pending rows (grouping.rs module doc), so this
                    // badge is the only signal that a collapsed group hides
                    // undecided connections — it must read as a badge, not
                    // as body text.
                    //
                    // Issue #31: this used to fill with neutralBackgroundColor
                    // under a neutralTextColor label. Under Fusion/Basic those
                    // two roles resolve to the identical orange, so the label
                    // vanished into its own fill. Border-only sidesteps the
                    // pairing entirely — neutralTextColor is already used bare
                    // (no fill) elsewhere on this page, e.g. the verdict Label
                    // above, so it's proven to read against the row background.
                    visible: row.isGroupHeader && row.groupPending > 0
                    color: "transparent"
                    border.color: Kirigami.Theme.neutralTextColor
                    border.width: 1
                    radius: height / 2
                    implicitWidth: pendingBadgeLabel.implicitWidth + Kirigami.Units.largeSpacing
                    implicitHeight: pendingBadgeLabel.implicitHeight + Kirigami.Units.smallSpacing
                    Layout.alignment: Qt.AlignVCenter

                    Controls.Label {
                        id: pendingBadgeLabel
                        anchors.centerIn: parent
                        text: row.groupPending + " pending"
                        color: Kirigami.Theme.neutralTextColor
                        font: Kirigami.Theme.smallFont
                    }
                }

                // Issue #18: per-process batch actions. Only on top-level
                // process headers (depth 0) with at least one pending
                // descendant — a domain header or a fully-decided process has
                // nothing to batch-act on.
                RowLayout {
                    visible: row.isGroupHeader && row.depth === 0 && row.groupPending > 0
                    spacing: Kirigami.Units.smallSpacing

                    Controls.Button {
                        flat: true
                        enabled: !row.submitted
                        text: "Allow all (" + row.groupPending + ")"
                        icon.name: "dialog-ok-apply"
                        onClicked: row.decideOnce("allow", true)
                    }
                    Controls.Button {
                        id: batchDenyButton
                        flat: true
                        enabled: !row.submitted
                        text: "Deny all (" + row.groupPending + ")"
                        icon.name: "edit-delete-remove"
                        onClicked: row.decideOnce("deny", true)
                        Accessible.description: row.denyAllText

                        // How long these Denies last (see the row Deny's
                        // tooltip below).
                        Controls.ToolTip {
                            visible: batchDenyButton.hovered
                            delay: Kirigami.Units.toolTipDelay
                            contentItem: Controls.Label {
                                textFormat: Text.PlainText
                                wrapMode: Text.Wrap
                                color: palette.toolTipText
                                text: row.denyAllText
                            }
                        }
                    }
                }

                // Issue #18: inline Allow/Deny on pending leaf rows, so a
                // decision no longer requires opening the inspector sheet.
                // Allow submits the sheet's defaults; Deny remembers until the
                // firewall restarts where it can (see InlineVerdicts.qml).
                // Disabled after one
                // click (see `row.submitted`'s doc comment) until the round
                // trip flips `pending` and resets the guard.
                RowLayout {
                    visible: !row.isGroupHeader && row.pending
                    spacing: Kirigami.Units.smallSpacing

                    Controls.Button {
                        flat: true
                        enabled: !row.submitted
                        text: "Allow"
                        icon.name: "dialog-ok-apply"
                        onClicked: row.decideOnce("allow", false)
                    }
                    Controls.Button {
                        id: rowDenyButton
                        flat: true
                        enabled: !row.submitted
                        text: "Deny"
                        icon.name: "edit-delete-remove"
                        onClicked: row.decideOnce("deny", false)
                        Accessible.description: row.denyText

                        // How long this Deny lasts. An explicit PlainText
                        // contentItem, never the attached `ToolTip.text`
                        // (issue #51).
                        Controls.ToolTip {
                            visible: rowDenyButton.hovered
                            delay: Kirigami.Units.toolTipDelay
                            contentItem: Controls.Label {
                                textFormat: Text.PlainText
                                wrapMode: Text.Wrap
                                // The style's own tooltip text uses the
                                // tooltip palette, not the window one.
                                color: palette.toolTipText
                                text: row.denyText
                            }
                        }
                    }
                    DecideLaterButton {
                        objectName: "decideLaterButton"
                        visible: row.decideLaterOffered
                        enabled: !row.submitted
                        onClicked: row.decideOnce("later", false)
                    }
                }
            }
        }
    }

    // Issue #49: the inspector works from a copy of the row taken when it
    // opened. If the bridge withdraws that prompt afterwards (the daemon's ask
    // timed out, rows were cleared, the service restarted), the copy would still
    // say pending and Allow/Deny would act on a row that no longer exists: the
    // bridge rejects the verdict while the sheet closes as if it had worked.
    // A snapshot clears the model and then re-inserts the rows that are still
    // pending, including this one, so a withdrawn prompt comes back if the very
    // same id is pending again. Row ids carry the bridge session, so a later
    // row that reuses the wire id is a different id and cannot revive it. The
    // inspector itself stays open.
    function inspectedRowDecidable() {
        // A closed feed can't carry a verdict, whatever the model still holds
        // from the lost session.
        if (page.bridgeFeed && page.bridgeFeed.ok === false) {
            return false;
        }
        return !!page.model && page.model.isPendingRow(page.inspectId);
    }

    function recheckInspectedRow() {
        const decidable = page.inspectedRowDecidable();
        if (page.inspectPending && !decidable) {
            page.inspectPending = false;
            page.inspectNoLongerPending = true;
        } else if (page.inspectNoLongerPending && decidable) {
            page.inspectPending = true;
            page.inspectNoLongerPending = false;
        }
    }

    function openInspector(row) {
        page.inspectNoLongerPending = false;
        page.inspectId = row.rowId;
        page.inspectProcess = row.process;
        page.inspectHost = row.host;
        page.inspectPort = row.port;
        page.inspectProtocol = row.protocol;
        page.inspectVerdict = row.verdict;
        page.inspectPending = row.pending;
        page.inspectMatchedRule = row.matchedRule;
        page.inspectMatchedRuleDisplay = row.matchedRuleDisplay;
        page.inspectDeadlineMs = row.answerDeadlineMs > 0 ? row.answerDeadlineMs : -1;
        page.inspectOutcomeText = row.outcomeText || "";
        page.inspectMakeRuleOffered = row.makeRuleOffered === true;
        page.applyRowDetails(row.rowId);
        page.inspectAppBoundRules = verdictHelper.rowAppBoundRules(row.rowId);
        page.inspectDecideLater = verdictHelper.rowDecideLater(row.rowId);
        // The row may be a stale pending one, and with the connection already
        // down no `ok` change follows to catch it.
        page.recheckInspectedRow();
        inspector.open();
    }

    // Parity 2: pull the destination IP for the insight panel. Best-effort —
    // malformed/missing JSON degrades to a blank value rather than throwing,
    // since this is a decorative side-channel, never a blocker.
    function applyRowDetails(id) {
        page.inspectIp = "";
        page.inspectBindableProcessPath = false;
        page.inspectAlsoListedByDefault = false;
        if (!page.model) {
            return;
        }
        try {
            const details = JSON.parse(page.model.rowDetailsJson(id));
            page.inspectIp = details.dstIp || "";
            page.inspectBindableProcessPath = details.bindableProcessPath === true;
            page.inspectAlsoListedByDefault = details.alsoListedByDefault === true;
        } catch (e) {
            // Leave the defaults above.
        }
    }

    // Row inspector. For a *pending* row this is where the decision prompt lives
    // (Task 7 wires the verdict actions in); for decided rows it is read-only
    // detail. Kept as an OverlaySheet so it behaves the same at every width.
    SizedOverlaySheet {
        id: inspector
        title: page.inspectProcess

        ColumnLayout {
            Layout.preferredWidth: inspector.preferredWidth
            spacing: Kirigami.Units.largeSpacing

            Kirigami.FormLayout {
                Layout.fillWidth: true
                Controls.Label {
                    Kirigami.FormData.label: "Host"
                    textFormat: Text.PlainText
                    text: page.inspectHost
                }
                Controls.Label {
                    Kirigami.FormData.label: "Destination IP"
                    textFormat: Text.PlainText
                    text: page.inspectIp.length > 0 ? page.inspectIp : "unavailable"
                    elide: Text.ElideMiddle
                }
                Controls.Label {
                    Kirigami.FormData.label: "Port"
                    text: page.inspectPort
                }
                Controls.Label {
                    Kirigami.FormData.label: "Protocol"
                    textFormat: Text.PlainText
                    text: page.inspectProtocol
                }
                Controls.Label {
                    Kirigami.FormData.label: "Verdict"
                    textFormat: Text.PlainText
                    text: page.inspectVerdictText
                    color: page.inspectNoLongerPending
                        ? Kirigami.Theme.disabledTextColor
                        : page.verdictColor(page.inspectVerdict)
                }
                Controls.Label {
                    Kirigami.FormData.label: "Matched rule"
                    textFormat: Text.PlainText
                    text: page.inspectMatchedRuleDisplay
                    elide: Text.ElideMiddle
                }
            }

            // Rule-match diagnostics (Little-Snitch-parity "which rule
            // decided this connection"). Only shown when a specific rule
            // name is known — a pending row ("awaiting decision") or a
            // decided row with no rule name on record ("default action")
            // have nothing to jump to.
            Controls.Button {
                Layout.fillWidth: true
                visible: page.inspectMatchedRule.length > 0
                text: "Show rule"
                icon.name: "view-list-details"
                onClicked: {
                    page.showRuleRequested(page.inspectMatchedRule);
                    inspector.close();
                }
            }

            Controls.Button {
                objectName: "simulateConnectionButton"
                Layout.fillWidth: true
                text: "Simulate this connection"
                icon.name: "system-run"
                onClicked: {
                    page.simulateConnectionRequested(page.model.simulationPrefillJson(page.inspectId));
                    inspector.close();
                }
            }

            // Issue #49: replaces the decision sheet below once its request is
            // gone, so a late click can't be mistaken for a decision. Worded
            // neutrally: the page can't tell a timeout from a verdict decided
            // elsewhere or a lost connection.
            Kirigami.InlineMessage {
                Layout.fillWidth: true
                visible: page.inspectNoLongerPending
                type: Kirigami.MessageType.Warning
                text: "This connection is no longer waiting for a decision. It may have "
                    + "timed out (opensnitchd then applies its default action), been decided "
                    + "elsewhere, or the connection to the background service may have been lost."
            }

            // Pending decision surface (Task 7). The countdown/timeout stays
            // server-side; these buttons call the bridge's verdict path once
            // pending_decision.rs is wired to the injected model.
            PendingDecisionSheet {
                id: pendingSheet
                Layout.fillWidth: true
                visible: page.inspectPending
                rowId: page.inspectId
                process: page.inspectProcess
                host: page.inspectHost
                remoteIp: page.inspectIp
                bindableProcessPath: page.inspectBindableProcessPath
                appBoundRules: page.inspectAppBoundRules
                deadlineMs: page.inspectDeadlineMs
                decideLater: page.inspectDecideLater
                bridgeFeed: page.bridgeFeed
                onDecided: inspector.close()
                onExplained: text => page.showPassiveNotice(text)
            }

            // E3: the bridge can't match a put-off row to the row the daemon
            // reports once its default action applied, so say there may be
            // two (plan 2026-10-08-default-applied-events.md).
            Controls.Label {
                id: alsoListedNote
                Layout.fillWidth: true
                visible: page.inspectAlsoListedByDefault
                wrapMode: Text.Wrap
                opacity: 0.7
                textFormat: Text.PlainText
                text: "The firewall may also list this connection, and its retries, separately "
                    + "as decided by its default action."
            }

            // Part C: a put-off connection can still get a rule, and so can
            // one the firewall's default action decided (E3).
            MakeRuleSheet {
                id: makeRuleSheet
                Layout.fillWidth: true
                visible: page.inspectMakeRuleOffered
                rowId: page.inspectId
                model: page.model
                bindableProcessPath: page.inspectBindableProcessPath
                blockedForFiveMinutes: page.inspectMatchedRule.length > 0
            }
        }
    }
}
