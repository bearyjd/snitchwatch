// What the tree's rule buttons do (Phase 2 step 4): sends their create and
// delete commands through ConnectionsModel, asks first for a program-wide
// rule, keeps the 1 s poll running while a command waits, and says how each
// command ended.
//
// The outcome shown is the model's `levelRuleFinished` signal (the bridge's
// RuleCommandResult), never an assumption that a send worked; a refusal shows
// the model's human-readable reason. All of that text is data, so it sits in a
// PlainText label. `header` is the bar the page puts above its list.
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

    // Commands sent and not yet ended, by "level|program|key".
    property var inFlight: ({})
    property int inFlightCount: 0
    // Last message, shown in the bar until dismissed.
    property string message: ""
    // The program-wide rule waiting for the dialog's answer, or null.
    property var asked: null

    // Exposed for the headless probes.
    property alias pollTimer: pollTimer
    property alias confirmDialog: confirmDialog
    readonly property string confirmText: confirmDialog.subtitle

    visible: actions.message !== ""
    spacing: Kirigami.Units.smallSpacing

    function nodeId(level, program, key) {
        return level + "|" + program + "|" + key;
    }

    function track(id, on) {
        const next = Object.assign({}, actions.inFlight);
        if (on) {
            next[id] = true;
        } else {
            delete next[id];
        }
        actions.inFlight = next;
        actions.inFlightCount = Object.keys(next).length;
    }

    // A button's choice. A program-wide rule (level 0) waits for the dialog;
    // domain and host rules go straight out.
    function create(level, program, key, label, choice) {
        if (level === 0) {
            actions.asked = { level: level, program: program, key: key, label: label, choice: choice };
            confirmDialog.open();
            return;
        }
        actions.send(level, program, key, choice, false);
    }

    function remove(level, program, key) {
        const why = actions.model.deleteLevelRule(level, program, key);
        actions.sent(level, program, key, why);
    }

    function send(level, program, key, choice, confirmed) {
        const why = actions.model.createLevelRule(level, program, key, choice, confirmed);
        actions.sent(level, program, key, why);
    }

    function sent(level, program, key, why) {
        if (why !== "") {
            actions.message = why;
            return;
        }
        actions.message = "";
        actions.track(actions.nodeId(level, program, key), true);
    }

    // Only the dialog's accept gets here.
    function accepted() {
        const a = actions.asked;
        actions.asked = null;
        if (a !== null) {
            actions.send(a.level, a.program, a.key, a.choice, true);
        }
    }

    Connections {
        target: actions.model
        ignoreUnknownSignals: true
        function onLevelRuleFinished(level, program, key, ending, status) {
            actions.track(actions.nodeId(level, program, key), false);
            actions.message = status;
        }
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
        subtitle: actions.asked === null ? "" : (actions.asked.choice === "deny" ? "Deny" : "Allow")
            + " any host for " + actions.asked.label + ". The rule lasts: "
            + Durations.labelFor(Durations.defaultToken) + "."
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
            onClicked: actions.message = ""
        }
    }
}
