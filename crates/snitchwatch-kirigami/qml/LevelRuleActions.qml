// What the tree's rule buttons do (Phase 2 step 4): sends their create and
// delete commands through ConnectionsModel, asks first for a program-wide
// rule, keeps the 1 s poll running while a command waits, and says how each
// command ended.
//
// The outcome shown is the model's `levelRuleFinished` signal (the bridge's
// RuleCommandResult), never an assumption that a send worked; a refusal shows
// the model's human-readable reason. All of that text is data, so it sits in a
// PlainText label. `header` is the bar the page puts above its list.
//
// Every message starts with the label of the node it is about, and each node
// keeps its own: a new command, or another node's answer, never clears or
// replaces a different node's message (the last few are kept, oldest dropped).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: actions

    // The ConnectionsModel (createLevelRule, deleteLevelRule, pollLevelRules,
    // levelRuleFinished).
    property var model: null

    // `make_rule::Ending`.
    readonly property int endingFailed: 0
    readonly property int endingDone: 1

    // Commands sent and not yet ended, by "level|program|key" -> the node's
    // label (an answer is attributed to the node it was sent for).
    property var inFlight: ({})
    property int inFlightCount: 0
    // Messages by node id, oldest first, and all of them as the bar shows
    // them (one per line) until dismissed.
    property var notes: ({})
    property string message: ""
    readonly property int maxNotes: 5
    // The program-wide rule waiting for the dialog's answer, or null.
    property var asked: null

    // Exposed for the headless probes.
    property alias pollTimer: pollTimer
    property alias confirmDialog: confirmDialog
    property alias confirmBodyLabel: confirmBody
    property alias confirmProgramLabel: confirmProgram
    readonly property string confirmText: actions.asked === null ? ""
        : (actions.asked.choice === "deny" ? "Deny" : "Allow") + " any host for " + actions.asked.label
          + ". The rule lasts: " + actions.asked.duration + "."

    visible: actions.message !== ""
    spacing: Kirigami.Units.smallSpacing

    function nodeId(level, program, key) {
        return level + "|" + program + "|" + key;
    }

    // `label` is the node's label to track it under, or null to stop.
    function track(id, label) {
        const next = Object.assign({}, actions.inFlight);
        if (label !== null) {
            next[id] = label;
        } else {
            delete next[id];
        }
        actions.inFlight = next;
        actions.inFlightCount = Object.keys(next).length;
    }

    // Sets node `id`'s message to "label: text" ("" removes it).
    function note(id, label, text) {
        const next = Object.assign({}, actions.notes);
        delete next[id];
        if (text !== "") {
            next[id] = label + ": " + text;
        }
        const ids = Object.keys(next);
        while (ids.length > actions.maxNotes) {
            delete next[ids.shift()];
        }
        actions.notes = next;
        actions.message = ids.map(i => next[i]).join("\n");
    }

    function dismiss() {
        actions.notes = ({});
        actions.message = "";
    }

    // A button's choice. A program-wide rule (level 0) waits for the dialog;
    // domain and host rules go straight out.
    function sessionValid(session) {
        return session === undefined || (session !== "" && actions.model
            && typeof actions.model.currentSourceSession === "function"
            && actions.model.currentSourceSession() === session);
    }

    function recheckAsked() {
        const a = actions.asked;
        if (a && !actions.sessionValid(a.session)) {
            actions.asked = null;
            confirmDialog.close();
            actions.note(actions.nodeId(a.level, a.program, a.key), a.label,
                         "The background service changed while you were deciding. Open the menu again.");
        }
    }

    function create(level, program, key, label, choice, sourceSession) {
        if (!actions.sessionValid(sourceSession)) return;
        if (level === 0) {
            // The duration is the model's, as `createLevelRule` will take it.
            // `durationToken` goes back with the confirm: the model refuses it
            // if Settings changed what was shown.
            const token = actions.model.levelRuleDuration();
            actions.asked = { level: level, program: program, key: key, label: label, choice: choice,
                              duration: Durations.labelFor(token), durationToken: token, session: sourceSession };
            confirmDialog.open();
            return;
        }
        actions.send(level, program, key, label, choice, false, "");
    }

    function remove(level, program, key, label, sourceSession) {
        if (!actions.sessionValid(sourceSession)) return;
        const why = actions.model.deleteLevelRule(level, program, key);
        actions.sent(level, program, key, label, why);
    }

    // `shownDuration`: the duration token the user was shown ("" if none was).
    function send(level, program, key, label, choice, confirmed, shownDuration) {
        const why = actions.model.createLevelRule(level, program, key, choice, confirmed, shownDuration);
        actions.sent(level, program, key, label, why);
    }

    function sent(level, program, key, label, why) {
        const id = actions.nodeId(level, program, key);
        actions.note(id, label, why);
        if (why === "") {
            actions.track(id, label);
        }
    }

    // Only the dialog's accept gets here.
    function accepted() {
        const a = actions.asked;
        actions.asked = null;
        if (a !== null && actions.sessionValid(a.session)) {
            actions.send(a.level, a.program, a.key, a.label, a.choice, true, a.durationToken);
        }
    }

    Connections {
        target: actions.model
        ignoreUnknownSignals: true
        function onModelReset() { actions.recheckAsked(); }
        function onDataChanged() { actions.recheckAsked(); }
        function onLevelRuleFinished(level, program, key, ending, status) {
            const id = actions.nodeId(level, program, key);
            const label = actions.inFlight[id] !== undefined ? actions.inFlight[id]
                : (key !== "" ? key : program);
            actions.track(id, null);
            actions.note(id, label, status);
        }
    }

    // A reconnect can precede its new snapshot; do not leave an old-session
    // confirmation actionable while that snapshot is still queued.
    Timer {
        interval: 200
        repeat: true
        running: actions.asked !== null && actions.asked.session !== undefined
        onTriggered: actions.recheckAsked()
    }

    // Commands with no answer in time end as "unknown" through the model.
    Timer {
        id: pollTimer
        interval: 1000
        repeat: true
        running: actions.inFlightCount > 0
        onTriggered: actions.model.pollLevelRules()
    }

    Kirigami.PromptDialog {
        id: confirmDialog
        title: actions.asked !== null && actions.asked.choice === "deny"
            ? "Deny every connection of this program?" : "Allow every connection of this program?"
        // The name is the process's; the file is what the rule binds to. Both
        // are data, so plain text (the dialog's own subtitle is not used).
        standardButtons: Kirigami.Dialog.Cancel
        customFooterActions: [
            Kirigami.Action {
                text: actions.asked !== null && actions.asked.choice === "deny" ? "Deny any host" : "Allow any host"
                icon.name: actions.asked !== null && actions.asked.choice === "deny" ? "edit-delete-remove" : "dialog-ok-apply"
                onTriggered: {
                    actions.accepted();
                    confirmDialog.close();
                }
            }
        ]
        onClosed: actions.asked = null
        preferredWidth: Kirigami.Units.gridUnit * 24

        // The dialog's content has no layout of its own, and a wrapping label
        // gets no width from it: one column, as wide as the dialog allows.
        ColumnLayout {
            id: confirmColumn
            objectName: "confirmColumn"
            spacing: Kirigami.Units.largeSpacing
            Controls.Label {
                id: confirmBody
                objectName: "confirmBody"
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                text: actions.confirmText
            }
            Controls.Label {
                id: confirmProgram
                objectName: "confirmProgram"
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.WrapAnywhere
                text: actions.asked === null ? "" : "Program file: " + actions.asked.program
            }
        }
    }

    RowLayout {
        Layout.fillWidth: true
        Layout.margins: Kirigami.Units.smallSpacing
        Controls.Label {
            objectName: "ruleMessage"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: actions.message
        }
        Controls.Button {
            flat: true
            text: "Dismiss"
            onClicked: actions.dismiss()
        }
    }
}
