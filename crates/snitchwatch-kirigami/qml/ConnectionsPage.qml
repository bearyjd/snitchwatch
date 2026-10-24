// Connections view: filtering, app/domain/host grouping and the inspector.
// Rust owns filtering and aggregates; QML renders the flattened projection.
// Header clicks expand groups; leaf clicks inspect a connection. Pending-row
// auto-selection updates the list without pushing a page or stealing focus.
// The inspector follows selection in a wide pane and opens a detail page below
// compactWidth. Menus belong to this page so live updates cannot destroy them.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.Page {
    id: page
    title: "Connections"
    padding: 0
    // PageRow would give this page a fixed column in a wide window.
    Kirigami.ColumnView.fillWidth: true

    // The page itself, for components declared inside it (their `page` would shadow the id).
    readonly property var pageRoot: page
    signal analysisExportRequested()

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property ConnectionsModel model

    // Injected by main.qml for the map above the list; null in isolated tests.
    property var geoModel: null
    property alias mapBand: listPane.mapBand
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
    // Exposed for the headless probes (tests/inline_verdict_qml.rs, ...).
    ConnectionActions { id: persistentActions; page: page.pageRoot }
    property alias connectionActions: persistentActions
    property alias inlineVerdicts: verdictHelper
    property alias levelRules: topBar.ruleActions

    // Issue #18: a row's inline Allow/Deny. Also the entry point
    // `tests/inline_verdict_qml.rs` drives, to exercise the click -> submit ->
    // verdict-message path without synthesizing a real mouse click.
    function submitInlineVerdict(rowId, choice, once) {
        return verdictHelper.submit(rowId, choice, once);
    }

    // Issue #18: a process header's "Allow all"/"Deny all".
    function submitBatchVerdict(processKey, choice, sourceSession) {
        return verdictHelper.submitBatch(processKey, choice, sourceSession);
    }

    function submitGroupVerdict(program, level, key, parentDomain, choice, sourceSession, once) {
        return verdictHelper.submitGroupBatch(program, level, key, parentDomain, choice, sourceSession, once);
    }

    // Below this page width the inspector is a pushed page, not a column.
    readonly property real compactWidth: Kirigami.Units.gridUnit * 45
    // The window's page stack; null without one (isolated tests: always a column).
    readonly property var hostStack: Controls.ApplicationWindow.window
        ? (Controls.ApplicationWindow.window.pageStack || null) : null
    StackNav { id: detailNav; stack: page.hostStack }
    readonly property bool compact: page.hostStack !== null && page.width < page.compactWidth
    // The pushed ConnectionDetailPage while one is open (compact mode).
    property var detailPage: null
    // The panel currently on screen: the pushed page's, else the column's.
    readonly property var activePanel: page.detailPage ? page.detailPage.panel : sideLoader.item
    onCompactChanged: {
        if (page.compact) {
            page.pushDetailPage();
        } else {
            page.closeDetailPage();
        }
    }

    // The selection's snapshot (InspectorState.qml), as `inspect*` for the
    // panel and the headless probes.
    InspectorState {
        id: inspect
        model: page.model
        bridgeFeed: page.bridgeFeed
        verdicts: verdictHelper
    }
    property alias inspecting: inspect.inspecting
    property alias inspectId: inspect.rowId
    property alias inspectProcess: inspect.process
    property alias inspectHost: inspect.host
    property alias inspectPort: inspect.port
    property alias inspectProtocol: inspect.protocol
    property alias inspectVerdict: inspect.verdict
    property alias inspectPending: inspect.pending
    property alias inspectNoLongerPending: inspect.noLongerPending
    property alias inspectVerdictText: inspect.verdictText
    property alias inspectDeadlineMs: inspect.deadlineMs
    property alias inspectOutcomeText: inspect.outcomeText
    property alias inspectMakeRuleOffered: inspect.makeRuleOffered
    property alias inspectDecideLater: inspect.decideLater
    property alias inspectIp: inspect.ip
    property alias inspectDetails: inspect.details
    property alias inspectSession: inspect.sourceSession
    property alias inspectSameSource: inspect.sameSource
    property alias inspectBindableProcessPath: inspect.bindableProcessPath
    property alias inspectAlsoListedByDefault: inspect.alsoListedByDefault
    property alias inspectAppBoundRules: inspect.appBoundRules
    property alias inspectMatchedRule: inspect.matchedRule
    property alias inspectMatchedRuleDisplay: inspect.matchedRuleDisplay
    // For the headless probes and main.qml (the panel on screen).
    readonly property var decisionSheet: page.activePanel ? page.activePanel.decisionSheet : null
    readonly property var makeRuleSheet: page.activePanel ? page.activePanel.makeRuleSheet : null
    // The window's MakeRuleController (main.qml), for the Make a rule sheet.
    property var makeRuleController: null
    property alias connectionList: listPane.view
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
    // the inspector, and never while one is showing (a panel or a half-filled
    // make-rule form must not change under the user's hands).
    Connections {
        target: page.model
        function onAutoSelectRequested(row) {
            // The row was just inserted: select it once the list has it. The
            // index would shift under later inserts, so queue the id (calls
            // coalesce, and a group header has none).
            const id = page.model.rowIdAt(row);
            if (id !== "" && page.autoIds.indexOf(id) < 0) {
                page.autoIds = page.autoIds.concat([id]);
                Qt.callLater(page.runAuto);
            }
        }
        function onGeographicSelectionRequested(id) {
            Qt.callLater(() => listPane.view.selectGeographicRow(id));
        }
        // Issue #49: the bridge removes a pending row when opensnitchd's ask
        // times out, a snapshot starts with ClearConnectionRows and then
        // re-inserts what is still pending, and a verdict decided elsewhere
        // arrives as an update. Flat mode reports these as rowsRemoved,
        // rowsInserted and dataChanged; so do grouped expand/collapse and
        // grouped filter changes. Every other mutation (clear, rows arriving
        // in grouped mode, flat filters) brackets a full reset.
        function onRowsRemoved() {
            inspect.recheck();
            page.clearIfRowGone();
        }
        function onRowsInserted() {
            inspect.recheck();
            page.clearIfRowGone();
        }
        function onDataChanged() {
            inspect.recheck();
            page.clearIfRowGone();
        }
        function onModelReset() {
            inspect.recheck();
            page.clearIfRowGone();
            // The reset drops the list's current index; once it has settled,
            // point it back at the selected row, wherever that now is.
            Qt.callLater(page.reanchor);
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
                inspect.recheck();
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

    // Heading, search and Grouped only: the title row has a fixed height.
    titleDelegate: ConnectionsHeader {
        model: page.model
        title: page.title
    }

    // Filters and rule actions, above the list (see ConnectionsTopBar.qml).
    header: ConnectionsTopBar {
        id: topBar
        model: page.model
        onAnalysisExportRequested: page.analysisExportRequested()
    }

    Controls.SplitView {
        anchors.fill: parent
        orientation: Qt.Horizontal

        MonitorPane {
            id: listPane
            Controls.SplitView.fillWidth: true
            Controls.SplitView.minimumWidth: Kirigami.Units.gridUnit * 18
            page: pageRoot
        }

        // The inspector column (compact mode: see ConnectionDetailPage).
        Controls.ScrollView {
            id: sideColumn
            visible: !page.compact
            Controls.SplitView.preferredWidth: Kirigami.Units.gridUnit * 22
            Controls.SplitView.minimumWidth: Kirigami.Units.gridUnit * 16
            contentWidth: availableWidth
            Loader {
                id: sideLoader
                active: !page.compact
                x: Kirigami.Units.largeSpacing
                width: sideColumn.availableWidth - Kirigami.Units.largeSpacing * 2
                sourceComponent: InspectorPanel {
                    page: pageRoot
                    width: sideLoader.width
                }
            }
        }
    }

    // Row click: select; when compact, push the detail page.
    function openInspector(row) {
        page.mapBand.mapView.highlightRows([row.rowId]);
        inspect.snapshot(row);
        page.pushDetailPage();
    }

    function pushDetailPage() {
        if (page.compact && page.inspecting && !page.detailPage) {
            page.detailPage = detailNav.push(detailPageComponent, { page: pageRoot });
        }
    }

    // Row ids auto-select asked for since the last `runAuto`.
    property var autoIds: []
    // Every pending row the user closed (#146): auto-select skips them until
    // decided or removed, so a dismissed prompt does not come back.
    property var dismissedIds: []
    // A pending arrival selects the OLDEST pending row (the one waiting
    // longest); any other id (the grouped re-anchor) selects that row itself.
    function runAuto() {
        const ids = page.autoIds;
        page.autoIds = [];
        page.dismissedIds = page.dismissedIds.filter(id => page.model.isPendingRow(id));
        const anyPending = ids.some(id => page.model.isPendingRow(id));
        const index = anyPending ? page.model.oldestPendingRowIndex(JSON.stringify(page.dismissedIds))
            : (ids.length > 0 ? page.model.visibleRowIndex(ids[ids.length - 1]) : -1);
        if (index >= 0) page.selectAuto(index);
    }

    // The pushed detail page left the stack (Back): the selection ends with it.
    Connections {
        target: page.hostStack
        ignoreUnknownSignals: true
        function onDepthChanged() {
            const detail = page.detailPage;
            for (let i = 0; detail && i < page.hostStack.depth; i++) {
                if (page.hostStack.get(i) === detail) {
                    return;
                }
            }
            if (detail) {
                page.detailPageGone();
            }
        }
    }

    function detailPageGone() {
        page.detailPage = null;
        page.dismissInspector();
    }

    function selectAuto(row) {
        const view = listPane.view;
        if (page.inspecting) {
            return;
        }
        view.forceLayout();
        view.currentIndex = row;
        const item = view.itemAtIndex(row);
        page.model.setCurrentRowId(item ? item.rowId : "");
        page.followSelection(item);
    }

    function reanchor() {
        const index = page.inspecting ? page.model.visibleRowIndex(page.inspectId) : -1;
        if (index >= 0 && listPane.view.currentIndex !== index) {
            listPane.view.currentIndex = index;
        }
    }

    Component {
        id: detailPageComponent
        ConnectionDetailPage {}
    }

    function closeDetailPage() {
        const detail = page.detailPage;
        page.detailPage = null;
        if (detail && page.hostStack) {
            page.hostStack.pop(page);
        }
    }

    // Close / finished action: clear the selection.
    function dismissInspector() {
        const closed = inspect.rowId;
        if (closed !== "" && page.dismissedIds.indexOf(closed) < 0) {
            page.dismissedIds = page.dismissedIds.concat([closed]);
        }
        inspect.clear();
        page.closeDetailPage();
        listPane.view.currentIndex = -1;
        // A prompt that came in while the panel was busy was not shown.
        Qt.callLater(page.selectOldestPending);
    }

    function selectOldestPending() {
        const index = page.model ? page.model.oldestPendingRowIndex(JSON.stringify(page.dismissedIds)) : -1;
        if (index >= 0) page.selectAuto(index);
    }

    // The list's current row changed (click, auto-select, or the model moving
    // it). A wide window shows it; a compact one waits for a click.
    function followSelection(item) {
        if (page.model && typeof page.model.destinationRowIdsJson === "function") {
            const ids=item ? JSON.parse(page.model.destinationRowIdsJson(listPane.view.currentIndex)) : [];
            page.mapBand.mapView.highlightRows(ids);
        }
        if (!item || item.isGroupHeader || item.rowId === page.inspectId) {
            return;
        }
        if (page.inspecting && page.model.visibleRowIndex(page.inspectId) < 0) {
            page.dismissInspector();
        } else if (!page.compact) {
            inspect.snapshot(item);
        }
    }

    // The selected row left the list (filter, age-out, collapsed group) or is
    // still pending but filtered away: nothing to show. A pending row that was
    // withdrawn keeps its "no longer waiting" notice instead (issue #49).
    function clearIfRowGone() {
        if (!page.inspecting || page.inspectNoLongerPending
                || page.model.visibleRowIndex(page.inspectId) >= 0) {
            return;
        }
        if (!page.inspectPending || page.model.isPendingRow(page.inspectId)) {
            page.dismissInspector();
        }
    }
}
