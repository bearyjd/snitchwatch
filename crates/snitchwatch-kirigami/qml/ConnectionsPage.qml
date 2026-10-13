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
// Little-Snitch-parity grouping (Process -> Domain -> Host -> connection): the
// header's "Grouped" switch drives `ConnectionsModel.setGrouped`. In grouped
// mode the same `ListView`/delegate renders a mix of row kinds — process
// headers (depth 0), domain headers (depth 1), host headers (depth 2), and
// leaf connection rows (depth 3) — distinguished by the `isGroupHeader`/`depth`
// roles the Rust model exposes; clicking a header toggles its expand state via
// `toggleProcessGroup`/`toggleDomainGroup`/`toggleHostGroup` instead of
// opening the inspector.
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

    // The page itself, for components declared inside it: their own `page`
    // property would shadow the `page` id in a `page: page` binding.
    readonly property var pageRoot: page

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
    // E3: `rowDetailsJson`'s `alsoListedByDefault` (the two-rows hint).
    property bool inspectAlsoListedByDefault: false
    // Whether the inspected row's bridge advertised app-bound rules
    // (`InlineVerdicts.rowAppBoundRules`); false until known.
    property bool inspectAppBoundRules: false
    // Exposed for the headless probes (tests/verdict_not_remembered_qml.rs,
    // tests/inline_verdict_qml.rs).
    property alias decisionSheet: inspector.decisionSheet
    property alias makeRuleSheet: inspector.makeRuleSheet
    property alias inspectorSheet: inspector
    // The window's MakeRuleController (main.qml), for the Make a rule sheet.
    property var makeRuleController: null
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
        // rowsInserted and dataChanged; so do grouped expand/collapse and
        // grouped filter changes. Every other mutation (clear, rows arriving
        // in grouped mode, flat filters) brackets a full reset.
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

    // Heading, search and Grouped only: the title row has a fixed height.
    titleDelegate: ConnectionsHeader {
        model: page.model
        title: page.title
    }

    // The wrapping filter switches and summary sit above the list, in the
    // page content, where their height is laid out rather than overflowed.
    header: ConnectionsFilterBar {
        id: filterBar
        model: page.model
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

        delegate: ConnectionRow {
            page: pageRoot
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

    // Row inspector (see ConnectionInspector.qml).
    ConnectionInspector {
        id: inspector
        page: pageRoot
    }
}
