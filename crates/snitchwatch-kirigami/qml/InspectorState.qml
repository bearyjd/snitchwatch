// The snapshot of the connection shown in the inspector panel, split out of
// ConnectionsPage.qml (monitor-tree step 5). The panel works from a copy of
// the row taken when it was selected; `recheck()` keeps that copy honest.
// ConnectionsPage re-exports every field as `inspect*` for the panel/probes.
import QtQuick

QtObject {
    id: state

    // Wiring from ConnectionsPage.
    property var model: null
    property var bridgeFeed: null
    // The page's InlineVerdicts (per-row bridge capabilities).
    property var verdicts: null

    // A connection is selected; the fields below describe it.
    property bool inspecting: false
    property string rowId: ""
    property string process: ""
    property string host: ""
    property int port: 0
    property string protocol: ""
    property string verdict: ""
    property bool pending: false
    // Issue #49: set when the row opened as pending has since gone away (see
    // `recheck`). Shown as a notice in place of the decision sheet.
    property bool noLongerPending: false
    // What the Verdict row says. `verdict` is the copy taken when it opened,
    // so for a withdrawn prompt it would still read "pending".
    readonly property string verdictText: state.noLongerPending
        ? "no longer pending"
        : (state.pending ? "pending"
           : state.outcomeText !== "" ? state.outcomeText : state.verdict)
    // Prompt-slot plan Part C, copied from the row: when the bridge answers it
    // (-1: never), a put-off row's label, whether "Make a rule…" is offered (put
    // off, or decided by the default action, E3), whether it takes "Decide later".
    property real deadlineMs: -1
    property string outcomeText: ""
    property bool makeRuleOffered: false
    property bool decideLater: false
    // Parity 2, from `ConnectionsModel.rowDetailsJson`. Per-connection byte
    // counters are deliberately not surfaced: the bridge reports a hardcoded 0
    // (issue #49). `bindableProcessPath` (issue #44): whether an answer for
    // this program can be remembered; `alsoListedByDefault` is E3's hint.
    property string ip: ""
    property bool bindableProcessPath: false
    property bool alsoListedByDefault: false
    // Whether the row's bridge advertised app-bound rules; false until known.
    property bool appBoundRules: false
    // Raw matched-rule name (empty when unknown: drives "Show rule") and its
    // friendly display string (never blank, see `matched_rule_display`).
    property string matchedRule: ""
    property string matchedRuleDisplay: ""

    // Copy `row` (a delegate's fields) in and select it.
    function snapshot(row) {
        state.inspecting = true;
        state.noLongerPending = false;
        state.rowId = row.rowId;
        state.process = row.process;
        state.host = row.host;
        state.port = row.port;
        state.protocol = row.protocol;
        state.verdict = row.verdict;
        state.pending = row.pending;
        state.matchedRule = row.matchedRule;
        state.matchedRuleDisplay = row.matchedRuleDisplay;
        state.deadlineMs = row.answerDeadlineMs > 0 ? row.answerDeadlineMs : -1;
        state.outcomeText = row.outcomeText || "";
        state.makeRuleOffered = row.makeRuleOffered === true;
        state.applyDetails(row.rowId);
        state.appBoundRules = state.verdicts.rowAppBoundRules(row.rowId);
        state.decideLater = state.verdicts.rowDecideLater(row.rowId);
        // The row may be a stale pending one, and with the connection already
        // down no `ok` change follows to catch it.
        state.recheck();
    }

    // Deselect. The rest of the copy is left; nothing shows it.
    function clear() {
        state.inspecting = false;
        state.rowId = "";
        state.noLongerPending = false;
    }

    // Best-effort: malformed/missing JSON degrades to blank values, since this
    // is a decorative side-channel, never a blocker.
    function applyDetails(id) {
        state.ip = "";
        state.bindableProcessPath = false;
        state.alsoListedByDefault = false;
        if (!state.model) {
            return;
        }
        try {
            const details = JSON.parse(state.model.rowDetailsJson(id));
            state.ip = details.dstIp || "";
            state.bindableProcessPath = details.bindableProcessPath === true;
            state.alsoListedByDefault = details.alsoListedByDefault === true;
        } catch (e) {
            // Leave the defaults above.
        }
    }

    // Issue #49: if the bridge withdraws a pending prompt (ask timed out, rows
    // cleared, service restarted), Allow/Deny would act on a row that no longer
    // exists. A snapshot clears the model and re-inserts the still-pending rows,
    // so a withdrawn prompt comes back if the very same id is pending again; row
    // ids carry the bridge session, so a reused wire id cannot revive it.
    function decidable() {
        // A closed feed can't carry a verdict, whatever the model still holds
        // from the lost session.
        if (state.bridgeFeed && state.bridgeFeed.ok === false) {
            return false;
        }
        return !!state.model && state.model.isPendingRow(state.rowId);
    }

    function recheck() {
        const ok = state.decidable();
        if (state.pending && !ok) {
            state.pending = false;
            state.noLongerPending = true;
        } else if (state.noLongerPending && ok) {
            state.pending = true;
            state.noLongerPending = false;
        }
    }
}
