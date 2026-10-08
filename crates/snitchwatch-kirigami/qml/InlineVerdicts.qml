// What the Connections page's inline buttons (a row's Allow/Deny, a process
// header's "Allow all"/"Deny all") send and say. Plan:
// docs/superpowers/plans/2026-10-08-inline-deny-until-restart.md.
//
// Scope is the sheet's default, "This host only". The duration is per row,
// from `ConnectionsModel.inlineDurationFor` (Rust `inline_deny.rs`):
//   * Allow sends "This time", the sheet's default.
//   * Deny deliberately doesn't. The daemon never stores a once-only rule, so
//     it would drop only the packet that asked, and the SYN the kernel resends
//     a second later would get through or be asked about again. It sends
//     "until_quit" (daemon "until restart"), a rule the bridge binds to the
//     program and this host, when the row's program file is known and its
//     bridge session advertised app-bound rules. Otherwise it can only send
//     "This time", and says why.
//
// `BridgeFeed.submitVerdict` builds and dispatches the typed verdict, so
// `pending_decision`'s Rust mapping stays the single source of the wire shape.
// Every text here is fixed, never built from connection data.
import QtQuick

QtObject {
    id: verdicts

    property var model: null
    // Null in isolated component tests; nothing is sent then.
    property var bridgeFeed: null

    // A once-only Deny's explanation, for the page to show.
    signal explained(string text)

    // Issue #44: why an answer for a row's program applies only to this
    // connection. The bridge's `RuleRefusal::describe` sentence (a test keeps
    // them equal), never the wire `reason`.
    readonly property string notRememberedSentence: "Snitchwatch couldn't identify this program's file, so this answer applies only to this connection."
    // The row's bridge didn't advertise app-bound rules (`rowAppBoundRules`).
    readonly property string bridgeTooOldSentence: "This firewall bridge is too old to block just this program, so Deny applies to this connection only."
    // The Deny couldn't be queued: the bridge connection is gone (the page's
    // "no longer waiting" banner names the same cause).
    readonly property string notSentSentence: "The connection to the background service was lost, so this Deny wasn't sent."
    // The same, before a click, while the feed reports no connection.
    readonly property string disconnectedText: "The connection to the background service was lost, so Deny can't be sent."

    // Whether `rowId`'s bridge session advertised app-bound rules, asked at
    // the time of use. Anything else, including a feed without the check,
    // means no: a bridge before #50/#71 would turn an inline Deny into an
    // all-apps rule.
    function rowAppBoundRules(rowId) {
        return verdicts.bridgeFeed !== null
            && typeof verdicts.bridgeFeed.appBoundRulesFor === "function"
            && verdicts.bridgeFeed.appBoundRulesFor(rowId) === true;
    }

    // Sends `choice` for `rowId`. Returns why a Deny applies to this
    // connection only — "not_sent", "program_unknown" or "bridge_too_old" —
    // else "". Leaves the explanation to the caller, so a batch explains once.
    function send(rowId, choice) {
        if (verdicts.bridgeFeed === null) {
            // Unreachable in the running app (main.qml always injects the
            // feed) — but the caller has already latched `row.submitted`, so
            // a silent return here would leave a permanently un-decidable
            // row with no trace of why. Never swallow this.
            console.warn("InlineVerdicts.send: no bridgeFeed; verdict dropped for", rowId);
            return "";
        }
        const appBound = verdicts.rowAppBoundRules(rowId);
        const duration = verdicts.model
            ? verdicts.model.inlineDurationFor(rowId, choice, appBound) : "this_time";
        const queued = verdicts.bridgeFeed.submitVerdict(rowId, choice, "this_host", duration);
        if (choice !== "deny") {
            return "";
        }
        if (queued === false) {
            return "not_sent";
        }
        if (duration !== "this_time") {
            return "";
        }
        return verdicts.model ? verdicts.model.inlineDenyFor(rowId, appBound) : "program_unknown";
    }

    // A row's inline Allow/Deny.
    function submit(rowId, choice) {
        verdicts.explain(verdicts.send(rowId, choice));
    }

    // Issue #18 batch actions: the same choice for every pending row under
    // process group `processKey`, each with its own duration. No new WS
    // protocol — this is just N SetVerdict messages.
    function submitBatch(processKey, choice, sourceSession) {
        if (!verdicts.model) {
            return;
        }
        let reason = "";
        try {
            const ids = JSON.parse(verdicts.model.pendingRowIdsForProcess(processKey));
            for (const id of ids) {
                // The synchronous model flush may have installed a replacement
                // service's snapshot since this header was displayed.
                if (sourceSession && !id.startsWith(sourceSession)) {
                    continue;
                }
                const why = verdicts.send(id, choice);
                // A lost connection outranks why a sent Deny was once-only.
                if (why === "not_sent" || reason === "") {
                    reason = why || reason;
                }
            }
        } catch (e) {
            // Malformed JSON from the model would be a Rust-side bug;
            // degrade to a no-op rather than throwing in the delegate, but
            // don't swallow it silently.
            console.warn("InlineVerdicts.submitBatch failed:", e);
        }
        verdicts.explain(reason);
    }

    function onceOnlySentence(reason) {
        switch (reason) {
        case "not_sent": return verdicts.notSentSentence;
        case "bridge_too_old": return verdicts.bridgeTooOldSentence;
        default: return verdicts.notRememberedSentence;
        }
    }

    function explain(reason) {
        if (reason !== "") {
            verdicts.explained(verdicts.onceOnlySentence(reason));
        }
    }

    // What a Deny would do for `rowId` now: "disconnected" while the feed
    // reports no connection, else `inlineDenyFor`'s answer. Reading `ok` here
    // also makes the delegates' text bindings follow it.
    function denyKind(rowId) {
        if (verdicts.bridgeFeed !== null && verdicts.bridgeFeed.ok === false) {
            return "disconnected";
        }
        return verdicts.model
            ? verdicts.model.inlineDenyFor(rowId, verdicts.rowAppBoundRules(rowId))
            : "program_unknown";
    }

    function textFor(kind, untilRestartText) {
        switch (kind) {
        case "until_restart": return untilRestartText;
        case "disconnected": return verdicts.disconnectedText;
        default: return verdicts.onceOnlySentence(kind);
        }
    }

    // A row's Deny: its tooltip and accessible description.
    function denyText(rowId) {
        return verdicts.textFor(verdicts.denyKind(rowId),
                                "Blocks this program from this host until the firewall restarts");
    }

    // A process header's "Deny all". A group is keyed by the program's path,
    // so its first pending row speaks for all of them.
    function denyAllText(processKey) {
        const rowId = verdicts.model ? verdicts.model.firstPendingRowIdForProcess(processKey) : "";
        return verdicts.textFor(verdicts.denyKind(rowId),
                                "Blocks this program from each of these hosts until the firewall restarts");
    }
}
