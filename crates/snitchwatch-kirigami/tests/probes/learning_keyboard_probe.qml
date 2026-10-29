// Probe for tests/learning_keyboard_qml.rs: the learning review page, its
// summary dialog, the learning banner and the batch's Cancel, over stand-in
// controllers that behave as the real ones do (a tick or a confirmation
// re-publishes the JSON, so delegates are rebuilt). The shared name guard is
// spliced in at `@a11y-guard`, the phases at `@phases`.
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 900
    height: 1500
    property var failures: []
    property int phase: 0
    property int stalls: 0
    readonly property int maxStalls: 200
    property int reviews: 0
    property bool lastRepeat: false
    readonly property int repeat: Qt.GroupSwitchModifier
    // @a11y-guard

    property var rows: [
        {
            key: "path:/tmp/a", programLabel: "a", programLabelUnverified: true,
            programPathTail: "/tmp/a", programPathFull: "/tmp/a", flags: [], connections: 7,
            firstSeenMs: 1000, lastSeenMs: 2000, distinct: 3, distinctExact: true,
            destinationsHidden: false, top: [], highFanOut: false, dualUse: false, suggested: false,
            choices: [
                { kind: "allow-any", title: "Allow any destination", hosts: [], note: null },
                { kind: "block", title: "Block this program", hosts: [], note: null }
            ],
            notSelectable: null, alreadyPresent: []
        },
        {
            key: "path:/usr/bin/app", programLabel: "app", programLabelUnverified: false,
            programPathTail: "/usr/bin/app", programPathFull: "/usr/bin/app", flags: [],
            connections: 5, firstSeenMs: 1000, lastSeenMs: 2000, distinct: 2, distinctExact: true,
            destinationsHidden: false, top: [], highFanOut: false, dualUse: false, suggested: true,
            choices: [
                { kind: "allow-hosts", title: "Allow only these destinations", hosts: ["b.example"], note: null },
                { kind: "allow-any", title: "Allow any destination", hosts: [], note: null },
                { kind: "block", title: "Block this program", hosts: [], note: null }
            ],
            notSelectable: null, alreadyPresent: []
        },
        {
            key: "name:daemon", programLabel: "daemon", programLabelUnverified: true,
            programPathTail: "daemon", programPathFull: "daemon", flags: [], connections: 1,
            firstSeenMs: 1000, lastSeenMs: 2000, distinct: 1, distinctExact: true,
            destinationsHidden: false, top: [], highFanOut: false, dualUse: false, suggested: false,
            choices: [], notSelectable: "cannot make a rule: the program path is unknown", alreadyPresent: []
        }
    ]

    QtObject {
        id: ctl
        property string rowsJson: JSON.stringify(probe.rows)
        property string ticksJson: "{}"
        property string noticesJson: JSON.stringify({ programs: 3 })
        property string summaryJson: "null"
        property string progressJson: ""
        property string statusText: ""
        property string fileNotice: ""
        property int tickCount: 0
        property bool applying: false
        property bool loaded: true
        property bool autoConfirm: false
        property var ticks: ({})
        property var confirmed: [false, false]
        property var calls: []
        function log(what) { calls = calls.concat([what]); }
        function show() {
            summaryJson = JSON.stringify({ count: 3, droppedNames: 0, confirmed: ctl.confirmed,
                lines: ["Allow a to connect anywhere.", "Block app.", "Allow daemon."],
                extras: ["a: blocks need care. I understand.", "app: any destination. I understand."] });
        }
        function refresh() { log("refresh"); }
        function setTick(key, kind) {
            log("tick:" + key + ":" + kind);
            const next = Object.assign({}, ticks);
            if (kind === "") delete next[key]; else next[key] = kind;
            ticks = next;
            ticksJson = JSON.stringify(next);
            tickCount = Object.keys(next).length;
        }
        function selectSuggested() { log("suggest"); }
        function untickAll() { log("untick"); }
        function prepareSummary() { log("prepare"); confirmed = [autoConfirm, autoConfirm]; show(); }
        function discardSummary() { log("discard"); summaryJson = "null"; }
        function setExtraConfirmed(i, on) {
            log("confirm:" + i + ":" + on);
            const next = confirmed.slice();
            next[i] = on;
            confirmed = next;
            show();
        }
        function apply() { log("apply"); summaryJson = "null"; }
        function cancelApply() { log("cancel"); }
        function clearData() { log("clear"); }
        function destinations(key) { return "[]"; }
    }
    QtObject {
        id: set
        property bool learningActive: false
        property double learningEndsMs: 0
        property int programsSeen: 2
        property bool learningIncomplete: false
        property int droppedCount: 0
        property string learnedNotice: ""
        property string learnedPath: "/home/u/learned.json"
        property string learningStartError: ""
        property int stops: 0
        property var started: []
        function startLearning(hours) { started = started.concat([hours]); }
        function stopLearning() { stops++; }
        function refreshLearningStatus() {}
    }
    TestCase { id: tc; name: "learning"; when: false }

    ColumnLayout {
        anchors.fill: parent
        Controls.Button {
            id: start
            text: "Start here"
            Keys.priority: Keys.BeforeItem
            Keys.onPressed: event => probe.lastRepeat = event.isAutoRepeat
        }
        LearningBanner {
            id: banner
            Layout.fillWidth: true
            settings: set
            onReviewRequested: probe.reviews++
        }
        LearningReviewPage {
            id: page
            Layout.fillWidth: true
            Layout.fillHeight: true
            controller: ctl
            settings: set
        }
    }

    // @phases
}
