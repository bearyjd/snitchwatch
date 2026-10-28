// Headless probe for the learning review page (tests/learning_review_qml.rs).
// A check that fails throws, which Qt reports against this document's URL.
import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: win
    visible: true
    width: 900
    height: 3200

    readonly property string markup: "<b>bold</b> <img src='https://example.invalid/x.png'> &amp;"
    readonly property string shell: "qrc:/qt/qml/com/snitchwatch/shell/qml/"
    property string failure: ""
    property var page: null
    property var narrow: null

    property var rows: [
        {
            key: "path:/tmp/evil", programLabel: win.markup, programLabelUnverified: true,
            programPathTail: "/tmp/evil", programPathFull: "/tmp/" + win.markup,
            flags: ["runs from /tmp " + win.markup], connections: 7, firstSeenMs: 1000,
            lastSeenMs: 2000, distinct: 30, distinctExact: false, destinationsHidden: true,
            top: [], highFanOut: true, dualUse: false, suggested: false,
            choices: [
                { kind: "allow-any", title: "Allow any destination", hosts: [], note: null },
                { kind: "block", title: "BLOCK THIS PROGRAM", hosts: [], note: win.markup }
            ],
            notSelectable: null, alreadyPresent: []
        },
        {
            key: "path:/usr/bin/app", programLabel: "app", programLabelUnverified: false,
            programPathTail: "/usr/bin/app", programPathFull: "/usr/bin/app",
            flags: [], connections: 5, firstSeenMs: 1000, lastSeenMs: 2000, distinct: 2,
            distinctExact: true, destinationsHidden: false,
            top: [{ host: "a." + win.markup, count: 3 }, { host: "b.example", count: 2 }],
            highFanOut: false, dualUse: false, suggested: true,
            choices: [
                { kind: "allow-hosts", title: "Allow only these destinations",
                  hosts: ["a." + win.markup, "b.example"], note: null },
                { kind: "allow-any", title: "Allow any destination", hosts: [], note: null },
                { kind: "block", title: "BLOCK THIS PROGRAM", hosts: [], note: "Blocking this may stop networking or updates." }
            ],
            notSelectable: null, alreadyPresent: []
        },
        {
            key: "name:daemon", programLabel: "daemon", programLabelUnverified: true,
            programPathTail: "daemon", programPathFull: "daemon",
            flags: [], connections: 1, firstSeenMs: 1000, lastSeenMs: 2000, distinct: 1,
            distinctExact: true, destinationsHidden: false, top: [{ host: "x.example", count: 1 }],
            highFanOut: false, dualUse: false, suggested: false, choices: [],
            notSelectable: "cannot make a rule: the program path is unknown", alreadyPresent: []
        }
    ]

    QtObject {
        id: ctl
        property string rowsJson: JSON.stringify(win.rows)
        property string ticksJson: "{}"
        property string noticesJson: JSON.stringify({ programs: 3, incomplete: true, dropped: 2,
            droppedFlagged: 1, classAFull: true, dirsFull: 1, dirUnlistedConnections: 9, rulesRead: false })
        property string summaryJson: "null"
        property string progressJson: ""
        property string statusText: ""
        property string fileNotice: win.markup
        property int tickCount: 0
        property bool applying: false
        property bool loading: false
        property bool loaded: true
        property var calls: []
        function refresh() { calls.push("refresh"); }
        function setTick(key, kind) { calls.push("tick:" + key + ":" + kind); }
        function selectSuggested() { calls.push("suggest"); }
        function untickAll() { calls.push("untick"); }
        function prepareSummary() { calls.push("prepare"); }
        function discardSummary() { calls.push("discard"); }
        function setExtraConfirmed(i, on) { calls.push("confirm:" + i + ":" + on); }
        function apply() { calls.push("apply"); }
        function cancelApply() { calls.push("cancel"); }
        function clearData() { calls.push("clear"); }
        function destinations(key) { return JSON.stringify([{ host: win.markup, count: 4 }]); }
    }

    QtObject {
        id: set
        property bool learningActive: false
        property string learnedPath: "/home/u/" + win.markup
        property string learningStartError: ""
        property var started: []
        property int stops: 0
        function startLearning(hours) { started.push(hours); }
        function stopLearning() { stops++; }
    }

    TestCase { id: keys; name: "keys"; when: false }

    LearningReviewController { id: real }
    SettingsController { id: realSettings }

    function check(ok, what) {
        if (!ok) {
            throw new Error("learning review: " + what);
        }
    }

    function walk(item, visit, seen) {
        if (!item) return;
        seen = seen || [];
        if (seen.indexOf(item) >= 0) return;
        seen.push(item);
        visit(item);
        const kids = item.children ? item.children : [];
        for (let i = 0; i < kids.length; i++) walk(kids[i], visit, seen);
        if (item.contentItem && item.contentItem !== item) walk(item.contentItem, visit, seen);
        if (item.headerItem) walk(item.headerItem, visit, seen);
    }

    function collect(root, test) {
        const found = [];
        win.walk(root, item => { if (test(item)) found.push(item); });
        return found;
    }

    function plainTextShowing(text) {
        const hits = win.collect(win.page, item => item.text === text && item.textFormat !== undefined);
        check(hits.length > 0, "text not shown: " + text);
        for (const hit of hits) {
            check(hit.textFormat === Text.PlainText, "shown as " + hit.textFormat + ": " + text);
        }
    }

    function named(name) {
        const hits = win.collect(win.page, item => item.objectName === name);
        check(hits.length === 1, name + ": " + hits.length);
        return hits[0];
    }

    function checkBoxes() {
        return win.collect(win.page, item => item.objectName !== undefined
            && String(item.objectName).indexOf("tick-") === 0);
    }

    function checkRealController() {
        check(real.rowsJson === "[]" && real.ticksJson === "{}" && real.summaryJson === "null",
              "a new controller is not empty");
        check(real.tickCount === 0 && !real.applying && !real.loading && !real.loaded, "defaults");
        real.setTick("path:/x", "allow-any");
        check(real.statusText !== "" && real.tickCount === 0, "an unknown row was ticked");
        real.setTick("path:/x", "bogus");
        check(real.tickCount === 0, "an unknown kind was ticked");
        real.selectSuggested();
        check(real.tickCount === 0, "suggested ticked on an empty record");
        real.prepareSummary();
        check(real.statusText === "Nothing is ticked.", "prepare: " + real.statusText);
        real.apply();
        check(!real.applying && real.statusText === "The summary has not been prepared.",
              "apply without a summary: " + real.statusText);
        check(real.destinations("path:/x") === "[]", "destinations of nothing");
        real.cancelApply();
        check(typeof realSettings.startLearning === "function"
              && realSettings.learningStartError === "", "SettingsController lacks Start");
    }

    // Nothing clips at a narrow width: every button of the two button rows
    // ends inside its row, and the rows are as tall as their content.
    function checkNarrow() {
        for (const name of ["learningButtonFlow", "learningStartFlow"]) {
            const hits = win.collect(win.narrow, item => item.objectName === name);
            check(hits.length === 1, "narrow " + name + ": " + hits.length);
            const flow = hits[0];
            check(flow.width <= 360, "the row is wider than the page: " + flow.width);
            for (let i = 0; i < flow.children.length; i++) {
                const child = flow.children[i];
                if (!child.visible) continue;
                check(child.x + child.width <= flow.width + 1,
                      name + " clips child " + i + ": " + (child.x + child.width) + " > " + flow.width);
                check(child.y + child.height <= flow.height + 1, name + " is too short for child " + i);
            }
        }
    }

    function checkPage() {
        const page = win.page;
        check(ctl.calls.indexOf("refresh") >= 0 && ctl.calls.indexOf("discard") >= 0
              && ctl.calls.indexOf("discard") < ctl.calls.indexOf("refresh"),
              "a reopened page discards a stale summary before it refreshes: " + ctl.calls);

        // Nothing is ticked, and every row offers its choices.
        const boxes = win.checkBoxes();
        check(boxes.length === 5, "tick boxes: " + boxes.length);
        for (const box of boxes) check(!box.checked, "a box starts ticked");
        check(win.named("learningReviewButton").enabled === false, "Review enabled with nothing ticked");

        // Hostile text is shown as text.
        win.plainTextShowing(win.markup);
        win.plainTextShowing("/tmp/" + win.markup);
        win.plainTextShowing("runs from /tmp " + win.markup);
        win.plainTextShowing("Only: a." + win.markup + ", b.example");
        win.plainTextShowing("a." + win.markup + "  (3)");
        win.plainTextShowing("cannot make a rule: the program path is unknown");
        win.plainTextShowing("Programs recorded: 3. Saved in /home/u/" + win.markup + ".");
        // The limits are all there.
        check(win.named("learningLimits").lines.length === 8, "limits");
        for (const line of win.named("learningLimits").lines) win.plainTextShowing(line);
        // The 0a marker.
        win.plainTextShowing(win.markup + " ?");

        // A tick is the owner's click; it asks the controller.
        boxes[0].toggle();
        boxes[0].toggled();
        check(ctl.calls[ctl.calls.length - 1] === "tick:path:/tmp/evil:allow-any", "tick: " + ctl.calls);
        // The box shows the controller's state, not the click: nothing was
        // ticked there, so it is not drawn ticked.
        check(!boxes[0].checked, "a refused tick is drawn as ticked");
        check(boxes[0].Accessible.name.indexOf("Allow any destination") === 0
              && boxes[0].Accessible.name.indexOf(win.markup) > 0
              && boxes[0].Accessible.name.indexOf("/tmp/") > 0
              && boxes[0].Accessible.name.indexOf("name unverified") > 0, "tick name: " + boxes[0].Accessible.name);

        // The explicit buttons.
        win.named("learningSuggestButton").clicked();
        check(ctl.calls[ctl.calls.length - 1] === "suggest", "suggest");

        // High fan-out: the list is on demand.
        const showMore = win.collect(page, item => item.clicked !== undefined && item.text === "Show the busiest destinations");
        check(showMore.length >= 1 && showMore[0].visible, "show-more button: " + showMore.length);
        showMore[0].clicked();
        win.plainTextShowing(win.markup + "  (4)");

        // Clear is refused while learning (button off) and allowed otherwise.
        check(win.named("learningClearButton").enabled, "Clear disabled when idle");
        set.learningActive = true;
        check(!win.named("learningClearButton").enabled, "Clear enabled while learning");
        check(win.named("learningStartPanel").stopButton.visible && !win.named("learningStartPanel").startButton.enabled
              && !win.named("learningStartPanel").lengthChoice.enabled, "start panel while running");
        win.named("learningStartPanel").stopButton.clicked();
        check(set.stops === 1, "stop");
        set.learningActive = false;
        ctl.applying = true;
        check(!win.named("learningClearButton").enabled && !win.named("learningSuggestButton").enabled, "buttons enabled while applying");
        ctl.applying = false;
        check(win.named("learningClearButton").enabled, "Clear still disabled");
        // The first click only asks; deleting needs the second.
        const callsBefore = ctl.calls.length;
        win.named("learningClearButton").clicked();
        check(ctl.calls.length === callsBefore, "Delete acted on the first click");
        check(win.named("learningDeleteConfirm").visible, "no confirmation was asked");
        win.named("learningDeleteNo").clicked();
        check(!win.named("learningDeleteConfirm").visible && ctl.calls.length === callsBefore, "Keep it");
        win.named("learningClearButton").clicked();
        win.named("learningDeleteYes").clicked();
        check(ctl.calls[ctl.calls.length - 1] === "clear", "clear");
        check(!win.named("learningDeleteConfirm").visible, "confirmation stays open");

        // Start: 24 hours by default, 7 days the longest; the honest sentence
        // is shown, and Start needs the owner's tick.
        const panel = win.named("learningStartPanel");
        check(panel.lengthChoice.currentIndex === 2 && panel.chosenHours === 24, "default length");
        check(panel.warning.text === "Everything is allowed once for the whole window, including software you did not mean to run. Deny rules and blocklists still apply.", "warning: " + panel.warning.text);
        check(!panel.startButton.enabled, "Start enabled before the owner ticked");
        panel.understood.checked = true;
        check(panel.startButton.enabled, "Start disabled when idle and understood");
        panel.startButton.clicked();
        check(!panel.understood.checked, "the tick was not reset after Start");
        check(set.started.length === 1 && set.started[0] === 24, "start: " + set.started);
        panel.lengthChoice.currentIndex = panel.lengthChoice.count - 1;
        check(panel.chosenHours === 168 && panel.chosenHours <= 7 * 24, "longest length");
        set.learningStartError = win.markup;
        win.plainTextShowing(win.markup);

        // Review: enabled only with a tick; the summary dialog repeats every
        // rule and keeps Apply off until every extra is confirmed.
        ctl.tickCount = 2;
        check(win.named("learningReviewButton").enabled, "Review disabled with ticks");
        win.named("learningReviewButton").clicked();
        check(ctl.calls[ctl.calls.length - 1] === "prepare", "prepare");
        const lines = ["Allow app (/usr/bin/app) to connect to a.example, on any port.",
                       "BLOCK /usr/bin/app: every connection it makes will be denied.", win.markup];
        ctl.summaryJson = JSON.stringify({ count: 3, lines: lines,
            extras: ["app (/usr/bin/app) Blocking this may stop networking or updates. I understand."],
            confirmed: [false], droppedNames: 0 });
        const dialog = page.summaryDialog;
        check(dialog.visible, "the summary did not open");
        // Escape / Go back is a discard, and a null summary hides the dialog.
        dialog.forceActiveFocus();
        keys.keyClick(Qt.Key_Escape);
        check(ctl.calls[ctl.calls.length - 1] === "discard", "Escape did not discard: " + ctl.calls);
        ctl.summaryJson = "null";
        check(!dialog.visible, "the dialog stays open without a summary");
        ctl.summaryJson = JSON.stringify({ count: 3, lines: lines,
            extras: ["app (/usr/bin/app) Blocking this may stop networking or updates. I understand."],
            confirmed: [false], droppedNames: 0 });
        check(dialog.visible, "the dialog did not reopen");
        ctl.statusText = "The summary has not been prepared.";
        const status = win.collect(dialog.contentItem, item => item.objectName === "learningSummaryStatus");
        check(status.length === 1 && status[0].visible && status[0].text === ctl.statusText, "no status in the dialog");
        ctl.statusText = "";
        check(dialog.lineList.count === 3, "summary lines: " + dialog.lineList.count);
        check(dialog.lineList.height > 0, "the rule list has no height");
        const cells = win.collect(dialog.lineList.contentItem, item => item.text !== undefined && item.text !== "");
        check(cells.length >= 1 && cells[0].height > 0, "a rule line has no height");
        check(dialog.applyButton.enabled === false, "Apply enabled before the extra confirmation");
        const extra = win.collect(dialog.contentItem, item => item.objectName === "extra-0");
        check(extra.length === 1, "extra confirmation box");
        extra[0].toggled();
        check(ctl.calls[ctl.calls.length - 1].indexOf("confirm:0:") === 0, "confirm: " + ctl.calls);
        ctl.summaryJson = JSON.stringify({ count: 3, lines: lines, extras: ["x"],
            confirmed: [true], droppedNames: 0 });
        check(dialog.applyButton.enabled, "Apply disabled after every confirmation");
        const applyText = win.collect(dialog.applyButton, item => item.text === "Apply 3 rules");
        check(applyText.length === 1 && applyText[0].textFormat === Text.PlainText, "Apply N rules");
        check(applyText[0].opacity === 1.0 && dialog.applyButton.Accessible.name === "Apply 3 rules",
              "apply name/opacity: " + dialog.applyButton.Accessible.name);
        const extraBox = win.collect(dialog.contentItem, item => item.objectName === "extra-0")[0];
        check(extraBox.Accessible.name === "Confirm: x", "extra name: " + extraBox.Accessible.name);
        // Return alone never applies.
        const beforeReturn = ctl.calls.length;
        dialog.forceActiveFocus();
        keys.keyClick(Qt.Key_Return);
        keys.keyClick(Qt.Key_Enter);
        check(ctl.calls.length === beforeReturn, "Return acted: " + ctl.calls.slice(beforeReturn));
        const before = ctl.calls.length;
        dialog.applyButton.clicked();
        check(ctl.calls.length === before + 1 && ctl.calls[before] === "apply", "apply: " + ctl.calls);
        // The same click path is the only one: nothing else applied.
        check(ctl.calls.filter(c => c === "apply").length === 1, "apply count");

        // Cancel is there whenever a batch runs, with or without progress yet.
        ctl.summaryJson = "null";
        ctl.progressJson = "";
        ctl.applying = true;
        const cancel = win.collect(page, item => item.clicked !== undefined && item.text === "Cancel");
        check(cancel.length >= 1 && cancel[0].visible, "no Cancel while applying with no progress");
        cancel[0].clicked();
        check(ctl.calls[ctl.calls.length - 1] === "cancel", "cancel");
        ctl.applying = false;

        // Progress.
        ctl.progressJson = JSON.stringify({ state: "finished", summary: "2 added", rows: [
            { label: win.markup, status: "Added" }, { label: "b", status: "Refused: " + win.markup }] });
        win.plainTextShowing("2 added");
    }

    // Readiness gating (the #237 pattern): each stage waits until what it
    // checks exists, is shown and enabled, and its geometry held for a tick.
    property int stage: 1
    property int stalls: 0
    readonly property int maxStalls: 200
    property var geo: ({})
    function stable(key, item) {
        if (!item || !item.visible || !item.enabled || !(item.width > 0) || !(item.height > 0)) {
            win.geo[key] = "";
            return false;
        }
        const g = item.mapToItem(null, 0, 0);
        const geometry = [g.x, g.y, item.width, item.height].join(",");
        const same = geometry === win.geo[key];
        win.geo[key] = geometry;
        return same;
    }
    // "" when the page checks may run, else what they still wait for.
    function pageWaiting() {
        if (win.failure !== "") return "";
        if (!win.page || !win.narrow) return "the two pages";
        const boxes = win.checkBoxes();
        if (boxes.length !== 5) return "5 tick boxes (have " + boxes.length + ")";
        for (let i = 0; i < boxes.length; i++) {
            if (!win.stable("box" + i, boxes[i])) return "tick box " + i;
        }
        for (const name of ["learningButtonFlow", "learningStartFlow"]) {
            const hits = win.collect(win.narrow, item => item.objectName === name);
            if (hits.length !== 1 || !win.stable(name, hits[0])) return "narrow " + name;
        }
        return "";
    }
    // "" when the progress rows (a ListView's delegates) are drawn.
    function progressWaiting() {
        const rows = win.collect(win.page, item => item.text === "Refused: " + win.markup);
        return rows.length > 0 && win.stable("progress", rows[0]) ? "" : "the progress rows";
    }

    // The progress rows are a ListView: its delegates exist a moment later.
    function checkProgressRows() {
        win.plainTextShowing(win.markup);
        win.plainTextShowing("Refused: " + win.markup);
    }

    Timer {
        interval: 50
        running: true
        repeat: false
        onTriggered: {
            try {
                const component = Qt.createComponent(win.shell + "LearningReviewPage.qml");
                if (component.status !== Component.Ready) {
                    throw new Error(component.errorString());
                }
                win.page = component.createObject(win.contentItem, {
                    controller: ctl, settings: set, width: 880, height: 3100 });
                win.narrow = component.createObject(win.contentItem, {
                    controller: ctl, settings: set, x: 0, width: 360, height: 700 });
                if (win.page === null) throw new Error("page not created");
            } catch (e) {
                win.failure = String(e);
            } finally {
                stepTimer.start();
            }
        }
    }

    // Stage 1 checks the pages, stage 2 the progress rows; each runs once
    // its gate passes, or fails naming what it waited for.
    Timer {
        id: stepTimer
        interval: 100
        repeat: true
        onTriggered: {
            const waiting = win.stage === 1 ? win.pageWaiting() : win.progressWaiting();
            if (waiting !== "" && ++win.stalls < win.maxStalls) return;
            stop();
            try {
                if (waiting !== "") throw new Error("learning review: stage " + win.stage + " never ready for " + waiting);
                if (win.stage === 1) {
                    if (win.failure !== "") throw new Error("learning review: " + win.failure);
                    win.checkRealController();
                    win.checkNarrow();
                    win.checkPage();
                    win.stage = 2;
                    win.stalls = 0;
                    start();
                    return;
                }
                win.checkProgressRows();
                console.warn("LEARNING_REVIEW_PROBE_DONE");
                Qt.quit();
            } catch (e) {
                Qt.quit();
                throw e;
            }
        }
    }
}
