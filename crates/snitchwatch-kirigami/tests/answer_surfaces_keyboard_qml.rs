//! Q01 slice 2: the main window's answer surfaces by keyboard alone, with
//! REAL key events (QtTest `keyClick`), and every control named (the shared
//! runtime guard, `tests/support/a11y_guard.qmlpart`, with Qt's
//! accessibility layer on):
//!   * the decision sheet: Tab goes Scope, Duration, Allow, Deny; Space on
//!     Deny answers;
//!   * Make a rule: Space on "Make a rule…" opens the form and puts focus in
//!     it; Tab to Allow, Space makes the rule, and focus comes back to the
//!     button;
//!   * the prompt-slot banner: Tab reaches each of its actions once armed;
//!     Space on Deny answers;
//!   * the rule confirm dialog: Tab reaches its confirm and Cancel; Escape
//!     closes it without sending and gives focus back; Space confirms.
//!
//! Each phase waits on a readiness gate (#237). Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[path = "../src/accessibility_test.rs"]
mod accessibility;

const PROBE_URL: &str = "qrc:/answer_surfaces_probe.qml";
const DONE_MARKER: &str = "ANSWER_SURFACES_PROBE_DONE";

/// The shared guard helpers, spliced in at the `// @a11y-guard` line.
const GUARD: &str = include_str!("support/a11y_guard.qmlpart");

const QML: &str = r#"
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
    height: 1400
    property var failures: []
    property int phase: 0
    property int stalls: 0
    readonly property int maxStalls: 200
    property var order: []
    readonly property var keyOf: ({ Return: Qt.Key_Return, Enter: Qt.Key_Enter, Space: Qt.Key_Space, Escape: Qt.Key_Escape })
    // @a11y-guard

    QtObject {
        id: feedStub
        property bool ok: true
        property var submitted: []
        function submitVerdict(rowId, choice, scope, duration, bindable) {
            submitted = submitted.concat([[rowId, choice, scope, duration, bindable].join(":")]);
            return true;
        }
        function appBoundRulesFor(rowId) { return true; }
        // Recorded in the same list: no key may send either.
        function decideLater(rowId) { submitted = submitted.concat(["later:" + rowId]); return true; }
        function decideLaterFor(rowId) { return true; }
    }
    QtObject {
        id: statusStub
        property bool supported: true
        property bool held: false
        property string rowId: "1:held"
        property string text: "app has been waiting for your answer for 12 s."
    }
    QtObject {
        id: makeModel
        property var calls: []
        function makeRule(rowId, choice, scope, duration, requestId) {
            calls = calls.concat([[rowId, choice, scope, duration, requestId].join(":")]);
            return "";
        }
    }
    QtObject {
        id: makeController
        property string rowId: ""
        property string statusText: ""
        property bool busy: false
        // As the real controller: begin() is busy at once, which disables the
        // focused Allow/Deny button; only a refusal frees it again.
        function begin(rowId) {
            if (makeController.busy) return "";
            makeController.rowId = rowId;
            makeController.busy = true;
            return "req-" + (++makeController.requests);
        }
        function notSent(why) { makeController.busy = false; makeController.statusText = why; }
        property int requests: 0
    }
    QtObject {
        id: commands
        property var calls: []
        signal modelReset()
        signal levelRuleFinished(int level, string program, string key, int ending, string status)
        function currentSourceSession() { return "1:"; }
        function levelRuleDuration() { return "forever"; }
        function levelRuleScopeText(level, program, key) { return "Executable " + program + "; any host; any port; any protocol"; }
        function createLevelRuleForSession(level, program, key, choice, duration, session) {
            calls = calls.concat([[level, program, key, choice, duration, session].join(",")]);
            return "";
        }
        function pollLevelRules() {}
    }
    ConnectionsModel { id: connModel }

    ColumnLayout {
        width: parent.width
        Controls.Button { id: start; text: "Start here" }
        PendingDecisionSheet {
            id: decision
            objectName: "decisionSheet"
            Layout.fillWidth: true
            rowId: "1:asked"
            process: "app"
            host: "example.com"
            remoteIp: "93.184.216.34"
            bindableProcessPath: true
            appBoundRules: true
            bridgeFeed: feedStub
        }
        MakeRuleSheet {
            id: make
            objectName: "makeRuleSheet"
            Layout.fillWidth: true
            rowId: "1:putoff"
            model: makeModel
            controller: makeController
            bindableProcessPath: true
        }
        PromptSlotBanner {
            id: banner
            objectName: "promptSlotBanner"
            Layout.fillWidth: true
            status: statusStub
            model: connModel
            bridgeFeed: feedStub
            armDelayMs: 200
        }
        LevelRuleActions { id: rules; objectName: "levelRules"; model: commands; Layout.fillWidth: true }
    }
    TestCase { id: tc; name: "answers"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function focused() { return probe.activeFocusItem; }
    function text(item) { return item && item.text !== undefined ? String(item.text) : ""; }
    function inside(item, ancestor) {
        for (let it = item; it; it = it.parent) if (it === ancestor) return true;
        return false;
    }
    // Where the focus is: "<surface>/<text or objectName>".
    function where(item) {
        const owner = [decision, make, banner, rules].find(s => probe.inside(item, s));
        const label = probe.text(item) || (item ? item.objectName : "") || String(item);
        return (owner ? owner.objectName : "window") + "/" + label;
    }
    // Tab from `start` through the window once, recording where focus goes.
    function tabRound() {
        start.forceActiveFocus();
        const seen = [];
        for (let i = 0; i < 40; ++i) {
            tc.keyClick(Qt.Key_Tab);
            if (probe.focused() === start) break;
            seen.push(probe.where(probe.focused()));
        }
        return seen;
    }
    // Tab from `start` until focus is on `target` ("<surface>/<text>").
    function tabTo(target) {
        start.forceActiveFocus();
        for (let i = 0; i < 40; ++i) {
            tc.keyClick(Qt.Key_Tab);
            if (probe.where(probe.focused()) === target) return true;
        }
        return false;
    }
    function dialog() { return rules.confirmDialog; }
    // Press each key on whatever has focus; `sent()` must stay where it was.
    function pressSends(keys, sent, what) {
        const before = JSON.stringify(sent());
        for (const k of keys) {
            tc.keyClick(k);
            probe.expect(JSON.stringify(sent()) === before,
                         what + ": key " + k + " sent " + JSON.stringify(sent()) + " (was " + before + ")");
        }
    }
    function asked() { return feedStub.submitted; }
    function made() { return makeModel.calls; }
    function typed() { return commands.calls; }
    // Exactly one submission, equal to `expected`.
    function expectOnly(sent, expected, what) {
        probe.expect(sent().length === 1 && sent()[0] === expected,
                     what + ": expected exactly [" + expected + "], got " + JSON.stringify(sent()));
    }
    function sweepNamed(where, exact) {
        probe.sweep(where);
        for (const name of exact)
            probe.expect(probe.names[name] === true, where + ": no control named '" + name + "'");
    }

    // Phases 10-21: per key, open (wait closed), act (wait opened), check (wait closed).
    function dialogGate(phase) {
        if ((phase - 10) % 3 === 1) return probe.dialog().opened ? "" : "the confirm dialog open";
        return !probe.dialog().visible ? "" : "the confirm dialog closed";
    }
    function formControl(name) {
        return probe.find(make, it => it.objectName === name);
    }
    // "" once the opened form's four controls are visible, enabled and still.
    function formReady() {
        if (!make.form.visible) return "the Make a rule form visible";
        const controls = [probe.formControl("makeRuleScope"), probe.formControl("makeRuleDuration"),
                          probe.find(make.form, it => probe.text(it) === "Allow"),
                          probe.find(make.form, it => probe.text(it) === "Deny")];
        const names = ["Scope", "Duration", "Allow", "Deny"];
        for (let i = 0; i < 4; ++i) {
            if (!controls[i] || !controls[i].enabled) return "the form's " + names[i] + " enabled";
            if (!probe.stable("form" + names[i], controls[i])) return "the form's " + names[i] + " to stop moving";
        }
        return "";
    }

    function gate(phase) {
        switch (phase) {
        case 1: return banner.armed && probe.stable("deny", probe.find(banner, it => probe.text(it) === "Deny"))
            ? "" : "the armed banner";
        case 4: case 5: return probe.formReady();
        case 8: case 23: return probe.dialog().opened ? "" : "the confirm dialog open";
        case 9: case 22: return !probe.dialog().visible ? "" : "the confirm dialog closed";
        case 10: case 11: case 12: case 13: case 14: case 15: case 16: case 17: case 18: case 19: case 20: case 21:
            return probe.dialogGate(phase);
        default: return "";
        }
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                const waiting = probe.gate(probe.phase);
                if (waiting !== "") {
                    if (++probe.stalls < probe.maxStalls) return;
                    throw new Error("phase " + probe.phase + " never ready for " + waiting);
                }
                probe.stalls = 0;
                if (probe.phase === 0) {
                    probe.requestActivate();
                    connModel.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        { id: "1:held", process: "app", processPath: "/usr/bin/app", dstHost: "example.com",
                          dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp", direction: "outgoing",
                          action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0, matchedRule: null }] }));
                    statusStub.held = true;
                } else if (probe.phase === 1) {
                    const seen = probe.tabRound();
                    probe.order = seen;
                    const at = n => seen.indexOf(n);
                    for (const n of ["decisionSheet/Allow", "decisionSheet/Deny", "makeRuleSheet/Make a rule…",
                                     "promptSlotBanner/Allow once", "promptSlotBanner/Deny",
                                     "promptSlotBanner/Decide later", "promptSlotBanner/Review"])
                        probe.expect(at(n) >= 0, "Tab never reaches " + n + ": " + JSON.stringify(seen));
                    const scope = seen.findIndex(n => n.indexOf("decisionSheet/") === 0 && n.indexOf("scopeSelector") > 0);
                    const duration = seen.findIndex(n => n.indexOf("decisionSheet/") === 0 && n.indexOf("durationSelector") > 0);
                    probe.expect(scope >= 0 && scope < duration && duration < at("decisionSheet/Allow")
                                 && at("decisionSheet/Allow") < at("decisionSheet/Deny"),
                                 "the decision sheet's Tab order is not Scope, Duration, Allow, Deny: " + JSON.stringify(seen));
                    probe.sweepNamed("surfaces", ["Scope: This host only", "Make a rule…", "Allow once", "Review"]);
                } else if (probe.phase === 2) {
                    const none = [Qt.Key_Return, Qt.Key_Enter, Qt.Key_Escape];
                    probe.expect(feedStub.submitted.length === 0, "something was sent before any key: " + JSON.stringify(feedStub.submitted));
                    probe.expect(probe.tabTo("decisionSheet/scopeSelector"), "cannot Tab to the sheet's Scope");
                    probe.pressSends(none, probe.asked, "Return/Enter/Escape on Scope");
                    probe.expect(probe.tabTo("decisionSheet/durationSelector"), "cannot Tab to the sheet's Duration");
                    probe.pressSends(none, probe.asked, "Return/Enter/Escape on Duration");
                    probe.expect(probe.tabTo("decisionSheet/Allow"), "cannot Tab to the sheet's Allow");
                    probe.pressSends([Qt.Key_Return, Qt.Key_Enter, Qt.Key_Escape], probe.asked, "Return/Enter/Escape on Allow");
                    probe.expect(probe.tabTo("decisionSheet/Deny"), "cannot Tab to the sheet's Deny");
                    probe.pressSends([Qt.Key_Return, Qt.Key_Enter, Qt.Key_Escape], probe.asked, "Return/Enter/Escape on Deny");
                    tc.keyClick(Qt.Key_Space);
                    probe.expectOnly(probe.asked, "1:asked:deny:" + decision.scopeSelector.currentValue + ":"
                                     + decision.durationSelector.currentValue + ":true", "Space on Deny");
                    feedStub.submitted = [];
                    probe.expect(probe.tabTo("decisionSheet/Allow"), "cannot Tab back to the sheet's Allow");
                    tc.keyClick(Qt.Key_Space);
                    probe.expectOnly(probe.asked, "1:asked:allow:" + decision.scopeSelector.currentValue + ":"
                                     + decision.durationSelector.currentValue + ":true", "Space on Allow");
                    feedStub.submitted = [];
                } else if (probe.phase === 3) {
                    probe.expect(probe.tabTo("makeRuleSheet/Make a rule…"), "cannot Tab to Make a rule…");
                    probe.pressSends([Qt.Key_Return, Qt.Key_Enter, Qt.Key_Escape], probe.made, "Return/Enter/Escape on Make a rule…");
                    probe.expect(!make.form.visible, "Return, Enter or Escape opened the Make a rule form");
                    tc.keyClick(Qt.Key_Space);
                    probe.expect(make.form.visible, "Space did not open the Make a rule form");
                    probe.expect(probe.focused() === probe.formControl("makeRuleScope"),
                                 "focus did not land on the form's Scope: " + probe.where(probe.focused()));
                } else if (probe.phase === 4) {
                    probe.sweepNamed("make a rule form", ["Scope: This host only"]);
                    probe.expect(probe.formControl("makeRuleDuration").Accessible.name.indexOf("Duration: ") === 0,
                                 "the form's duration selector has no labelled name");
                    // Traversal in, through, and out of the form.
                    const walk = [];
                    for (let i = 0; i < 4; ++i) { tc.keyClick(Qt.Key_Tab); walk.push(probe.where(probe.focused())); }
                    probe.expect(JSON.stringify(walk) === JSON.stringify(["makeRuleSheet/makeRuleDuration",
                                 "makeRuleSheet/Allow", "makeRuleSheet/Deny", "promptSlotBanner/Allow once"]),
                                 "the form's Tab order is not Scope, Duration, Allow, Deny, then out: " + JSON.stringify(walk));
                    const none = [Qt.Key_Return, Qt.Key_Enter, Qt.Key_Escape];
                    for (const name of ["Scope", "Duration", "Allow", "Deny"]) {
                        const at = name === "Scope" ? "makeRuleSheet/makeRuleScope"
                            : name === "Duration" ? "makeRuleSheet/makeRuleDuration" : "makeRuleSheet/" + name;
                        probe.expect(probe.tabTo(at), "cannot Tab to the form's " + name);
                        probe.pressSends(none, probe.made, "Return/Enter/Escape on the form's " + name);
                        probe.expect(make.form.visible, "a key closed the form from its " + name);
                    }
                    probe.expect(probe.where(probe.focused()) === "makeRuleSheet/Deny", "focus is not on the form's Deny");
                    tc.keyClick(Qt.Key_Space);
                    probe.expectOnly(probe.made, "1:putoff:deny:" + probe.formControl("makeRuleScope").currentValue + ":"
                                     + probe.formControl("makeRuleDuration").currentValue + ":req-1", "Space on the form's Deny");
                    probe.expect(!make.form.visible && probe.focused() === make.openButton,
                                 "focus did not come back to Make a rule…: " + probe.where(probe.focused()));
                    makeModel.calls = [];
                    makeController.busy = false;
                    tc.keyClick(Qt.Key_Space);
                    probe.expect(make.form.visible, "Space did not reopen the Make a rule form");
                } else if (probe.phase === 5) {
                    probe.expect(probe.tabTo("makeRuleSheet/Allow"), "cannot Tab to the form's Allow");
                    tc.keyClick(Qt.Key_Space);
                    probe.expectOnly(probe.made, "1:putoff:allow:" + probe.formControl("makeRuleScope").currentValue + ":"
                                     + probe.formControl("makeRuleDuration").currentValue + ":req-2", "Space on the form's Allow");
                    probe.expect(!make.form.visible && probe.focused() === make.openButton,
                                 "focus did not come back to Make a rule…: " + probe.where(probe.focused()));
                } else if (probe.phase === 6) {
                    for (const name of ["Allow once", "Decide later", "Review"]) {
                        probe.expect(probe.tabTo("promptSlotBanner/" + name), "cannot Tab to the banner's " + name);
                        probe.pressSends([Qt.Key_Return, Qt.Key_Enter, Qt.Key_Escape], probe.asked,
                                         "Return/Enter/Escape on the banner's " + name);
                    }
                    probe.expect(probe.tabTo("promptSlotBanner/Deny"), "cannot Tab to the banner's Deny");
                    probe.pressSends([Qt.Key_Return, Qt.Key_Enter, Qt.Key_Escape], probe.asked, "Return/Enter/Escape on the banner's Deny");
                    tc.keyClick(Qt.Key_Space);
                    probe.expectOnly(probe.asked, "1:held:deny:this_host:until_restart:true", "Space on the banner's Deny");
                } else if (probe.phase === 7) {
                    start.forceActiveFocus();
                    rules.create(0, "/usr/bin/app", "", "app", "allow", "1:");
                } else if (probe.phase === 8) {
                    probe.sweepNamed("confirm dialog", ["Allow any host", "Cancel"]);
                    const seen = [];
                    for (let i = 0; i < 6; ++i) { tc.keyClick(Qt.Key_Tab); seen.push(probe.text(probe.focused())); }
                    probe.expect(seen.indexOf("Allow any host") >= 0 && seen.indexOf("Cancel") >= 0,
                                 "Tab in the dialog never reaches both buttons: " + JSON.stringify(seen));
                    tc.keyClick(Qt.Key_Escape);
                } else if (probe.phase === 9) {
                    probe.expect(commands.calls.length === 0, "Escape sent the rule: " + JSON.stringify(commands.calls));
                    probe.expect(probe.focused() === start, "Escape did not give focus back: " + probe.where(probe.focused()));
                } else if (probe.phase >= 10 && probe.phase < 22) {
                    // Each key on Cancel gets its own freshly opened dialog:
                    // open, Tab to Cancel (focus asserted), press, close.
                    const k = ["Return", "Enter", "Escape", "Space"][Math.floor((probe.phase - 10) / 3)];
                    const sub = (probe.phase - 10) % 3;
                    if (sub === 0) {
                        rules.create(0, "/usr/bin/app", "", "app", "allow", "1:");
                    } else if (sub === 1) {
                        for (let i = 0; i < 6 && probe.text(probe.focused()) !== "Cancel"; ++i)
                            tc.keyClick(Qt.Key_Tab);
                        probe.expect(probe.text(probe.focused()) === "Cancel", k + ": cannot Tab to Cancel (at " + probe.where(probe.focused()) + ")");
                        probe.expect(probe.dialog().visible, k + ": the dialog is not open");
                        tc.keyClick(probe.keyOf[k]);
                        probe.expect(commands.calls.length === 0, k + " on Cancel sent " + JSON.stringify(commands.calls));
                        if (k === "Space" || k === "Escape") {
                            probe.expect(!probe.dialog().opened, k + " on Cancel did not close the dialog");
                        } else if (probe.dialog().visible) {
                            // Return/Enter may leave it open: focus must still be on Cancel.
                            probe.expect(probe.text(probe.focused()) === "Cancel", k + ": focus left Cancel: " + probe.where(probe.focused()));
                            tc.keyClick(Qt.Key_Escape);
                        }
                    } else {
                        probe.expect(commands.calls.length === 0, k + " on Cancel sent " + JSON.stringify(commands.calls));
                        probe.expect(!probe.dialog().visible, k + ": the dialog is still open");
                    }
                } else if (probe.phase === 22) {
                    probe.expect(commands.calls.length === 0, "Cancel sent " + JSON.stringify(commands.calls));
                    rules.create(0, "/usr/bin/app", "", "app", "allow", "1:");
                } else if (probe.phase === 23) {
                    for (let i = 0; i < 6 && probe.text(probe.focused()) !== "Allow any host"; ++i)
                        tc.keyClick(Qt.Key_Tab);
                    probe.expect(probe.text(probe.focused()) === "Allow any host", "cannot Tab to Allow any host");
                    probe.expect(probe.dialog().visible, "the dialog closed before any key");
                    probe.pressSends([Qt.Key_Return, Qt.Key_Enter], probe.typed, "Return/Enter on Allow any host");
                    tc.keyClick(Qt.Key_Space);
                    probe.expectOnly(probe.typed, "0,/usr/bin/app,,allow,forever,1:", "Space on Allow any host");
                    const bad = Object.keys(probe.missing);
                    probe.expect(bad.length === 0, bad.length + " controls lack a meaningful Accessible.name: "
                        + bad.map(k => k + " (" + probe.missing[k] + ")").join("; "));
                    done = true;
                }
                probe.phase++;
            } catch (e) {
                probe.failures.push("phase " + probe.phase + " threw: " + e);
                done = true;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("answer surfaces probe: " + probe.failures.join("; "));
                    }
                    console.warn("ANSWER_SURFACES_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

#[test]
fn the_answer_surfaces_work_by_keyboard_and_name_every_control() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    // As an assistive technology would; Controls fill in their accessible
    // role and name only then.
    assert!(
        accessibility::ffi::activate_accessibility(),
        "Qt's accessibility layer did not turn on"
    );
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            let qml = QML.replace("    // @a11y-guard\n", GUARD);
            engine.load_data(&QByteArray::from(qml.as_str()), &QUrl::from(PROBE_URL));
        }
        if let Some(app) = app.as_mut() {
            app.exec();
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "probe root was null:\n{captured}"
    );
    let bad: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "answer surfaces probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
