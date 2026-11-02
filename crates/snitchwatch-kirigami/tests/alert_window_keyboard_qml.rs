//! Q01 slice 1: the standalone prompt (`AlertWindow.qml`) by keyboard alone,
//! with REAL key events (QtTest `keyPress`/`keyRelease`/`keyClick`, not
//! `handleKey` calls), and its controls named (the guard snippet in
//! `tests/support/a11y_guard_local.qmlpart`, to be replaced by the shared
//! `a11y_guard.qmlpart` once #253 merges). Run headless with
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

const PROBE_URL: &str = "qrc:/alert_keyboard_probe.qml";
const DONE_MARKER: &str = "ALERT_KEYBOARD_PROBE_DONE";
const GUARD: &str = include_str!("support/a11y_guard_local.qmlpart");

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    x: 900
    width: 200
    height: 200
    property var failures: []
    property int phase: 0
    property int stalls: 0
    // Key events spin the event loop: a tick must not re-enter a phase still typing.
    property bool inTick: false
    readonly property int maxStalls: 200
    // @a11y-guard

    QtObject {
        id: feedStub
        property var submitted: []
        property bool appBound: true
        function submitVerdict(rowId, choice, scope, duration, bindable) {
            submitted = submitted.concat([[rowId, choice, scope, duration, bindable].join(":")]);
            return true;
        }
        function appBoundRulesFor(rowId) { return feedStub.appBound; }
    }
    ConnectionsModel { id: connModel }
    AlertWindow {
        id: alert
        model: connModel
        bridgeFeed: feedStub
        armDelayMs: 200
    }
    TestCase { id: tc; name: "alert"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id) {
        return { id: id, process: "p-" + id, processPath: "/usr/bin/x", dstHost: "example.com",
                 dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                 answerDeadlineMs: Date.now() + 600000 };
    }
    function focused() { return alert.activeFocusItem; }
    function where(item) {
        if (!item) return "none";
        if (item === alert.keyCatcher) return "keys";
        return item.objectName === "alertAllow" ? "Allow" : item.objectName === "alertDeny" ? "Deny"
            : (item.text !== undefined ? String(item.text) : String(item).split("(")[0]);
    }
    readonly property var keyOf: ({ Return: Qt.Key_Return, Enter: Qt.Key_Enter, Space: Qt.Key_Space,
                                    Escape: Qt.Key_Escape, Tab: Qt.Key_Tab, Backtab: Qt.Key_Backtab })
    property string current: ""
    property int stage: 0
    property var stages: []
    readonly property var scopeLabels: ["Any host", "Any host on this domain", "This host only"]
    readonly property var durationLabels: ["Once", "5 minutes", "Until firewall restarts", "Forever"]
    readonly property var order: probe.scopeLabels.concat(probe.durationLabels, ["Allow", "Deny"])

    // Tab (real key events) until focus is on the control labelled `label`.
    function tabTo(label) {
        for (let i = 0; i < probe.order.length + 2; ++i) {
            if (probe.where(probe.focused()) === label) return true;
            tc.keyClick(Qt.Key_Tab);
        }
        return probe.where(probe.focused()) === label;
    }
    function step(name, what) {
        const at = what.indexOf(":");
        const verb = at < 0 ? what : what.slice(0, at);
        const arg = at < 0 ? "" : what.slice(at + 1);
        if (verb === "focus") alert.keyCatcher.forceActiveFocus();
        else if (verb === "tab") probe.expect(probe.tabTo(arg), name + ": cannot Tab to " + arg
                                              + " (at " + probe.where(probe.focused()) + ")");
        else if (verb === "click") tc.keyClick(probe.keyOf[arg]);
        else if (verb === "press") tc.keyPress(probe.keyOf[arg]);
        else if (verb === "release") tc.keyRelease(probe.keyOf[arg]);
        else if (verb === "none") probe.expect(feedStub.submitted.length === 0,
                                               name + ": sent too early: " + JSON.stringify(feedStub.submitted));
        else throw new Error("unknown step " + what);
    }
    function add(name, spec) {
        probe.stages.push(Object.assign({ name: name, row: false, rearm: false, pre: null, steps: [],
                                          expect: [], state: "", run: null }, spec));
    }
    // One stage = a start (a new row, a re-arm, or nothing), a readiness wait,
    // then the steps and the exact submissions they must have made.
    function build() {
        const D = "{r}:deny:this_host:forever:true", A = "{r}:allow:this_host:forever:true";
        probe.add("structure", { row: true, run: probe.structure });
        // Return/Enter/Space on a choice only selects it; Escape still denies.
        probe.add("Return on Any host", { row: true, steps: ["tab:Any host", "click:Return"], state: "any_host/forever" });
        probe.add("Enter on the domain scope", { row: true, steps: ["tab:Any host on this domain", "click:Enter"],
                  state: "any_host_on_domain/forever" });
        probe.add("Space on 5 minutes", { row: true, steps: ["tab:5 minutes", "click:Space"], state: "this_host/for_5_minutes" });
        probe.add("Return on Until firewall restarts", { row: true, steps: ["tab:Until firewall restarts", "click:Return"],
                  state: "this_host/until_restart" });
        probe.add("Return on the selected Forever", { row: true, steps: ["tab:Forever", "click:Return", "click:Enter"],
                  state: "this_host/forever" });
        probe.add("Escape on a scope choice", { row: true, steps: ["tab:Any host on this domain", "click:Escape"],
                  expect: [D] });
        probe.add("Escape on a duration choice", { row: true, steps: ["tab:Once", "click:Escape"], expect: [D] });
        // Verdict buttons.
        probe.add("Return on Allow", { row: true, steps: ["tab:Allow", "click:Return"], expect: [A] });
        probe.add("Enter on Allow", { row: true, steps: ["tab:Allow", "click:Enter"], expect: [A] });
        probe.add("Space on Allow", { row: true, steps: ["tab:Allow", "click:Space"], expect: [A] });
        probe.add("Escape on Allow", { row: true, steps: ["tab:Allow", "click:Escape"], expect: [D] });
        probe.add("Space on Deny", { row: true, steps: ["tab:Deny", "click:Space"], expect: [D] });
        probe.add("Return on Deny", { row: true, steps: ["tab:Deny", "click:Return"], expect: [D] });
        probe.add("Enter on Deny", { row: true, steps: ["tab:Deny", "click:Enter"], expect: [D] });
        // Return before any Tab is still Allow; Space on nothing is nothing.
        // (The previous row left the focus on Deny: a new row starts fresh.)
        probe.add("Return before any Tab", { row: true, steps: ["click:Space", "none", "click:Return"], expect: [A],
                  run: () => probe.expect(probe.focused() === alert.keyCatcher, "a new row kept the focus on " + probe.where(probe.focused())) });
        probe.add("Escape before any Tab", { row: true, steps: ["tab:Deny", "tab:Allow", "click:Escape"], expect: [D] });
        // Held keys, row replacement and re-arming.
        probe.add("Space held on Allow ...", { row: true, steps: ["tab:Allow", "press:Space"] });
        probe.add("... across a row replacement", { row: true,
                  steps: ["release:Space", "none", "tab:Allow", "click:Space"], expect: [A] });
        for (const k of ["Return", "Enter"]) {
            probe.add(k + " held on Allow answers the row ...", { row: true, steps: ["tab:Allow", "press:" + k], expect: [A] });
            probe.add("... and unflagged repeats of " + k + " on the next row", { row: true,
                      pre: () => { alert.keyCatcher.forceActiveFocus(); tc.keyPress(probe.keyOf[k]); tc.keyPress(probe.keyOf[k]); },
                      steps: ["press:" + k, "release:" + k, "none", "click:" + k], expect: [A] });
        }
        probe.add("Space held on Allow ... (2)", { row: true, steps: ["tab:Allow", "press:Space"] });
        probe.add("... across a duration re-arm", { rearm: true,
                  steps: ["release:Space", "none", "tab:Allow", "click:Space"],
                  expect: ["{r}:allow:any_host_on_domain:this_time:true"] });
        probe.add("Escape after a re-arm with focus on Allow ...", { row: true, steps: ["tab:Allow"] });
        probe.add("... still denies", { rearm: true, steps: ["click:Escape"], expect: ["{r}:deny:any_host_on_domain:this_time:true"] });
        for (const k of ["Space", "Return", "Enter", "Escape"]) {
            // Pressed while unarmed, still down when it arms, released on Allow.
            probe.add(k + " held from before arming", { row: true,
                      pre: () => { alert.keyCatcher.forceActiveFocus(); tc.keyPress(probe.keyOf[k]); tc.keyPress(probe.keyOf.Tab); },
                      run: () => probe.expect(probe.focused() === alert.keyCatcher,
                                              "Tab while unarmed moved focus to " + probe.where(probe.focused())),
                      steps: ["tab:Allow", "press:" + k, "none", "release:" + k, "none", "click:" + (k === "Escape" ? "Escape" : k === "Space" ? "Space" : k)],
                      expect: [k === "Escape" ? D : A] });
        }
        // A key that BEGAN while armed and is still down (no release) when the
        // alert re-arms for a replacement row: its unflagged repeats, with no
        // other press in between, are not an answer (#258 review).
        for (const [on, k] of [["Allow", "Return"], ["Allow", "Enter"], ["Deny", "Return"], ["Deny", "Enter"],
                               ["Any host", "Return"], ["Forever", "Enter"]]) {
            const answers = on === "Allow" || on === "Deny";
            probe.add(k + " pressed on " + on + " while armed ...", { row: true, steps: ["tab:" + on, "press:" + k],
                      expect: answers ? [on === "Allow" ? A : D] : [] });
            // An answer hides the alert; a key still down repeats while it
            // is back and unarmed (the hide forgot it), and is recorded again.
            probe.add("... unflagged " + k + " repeats on the replacement row", { row: true,
                      pre: answers ? () => { alert.keyCatcher.forceActiveFocus(); tc.keyPress(probe.keyOf[k]); } : null,
                      steps: ["press:" + k, "none", "press:" + k, "none", "release:" + k, "none", "click:" + k], expect: [A] });
        }
        // Space down on Allow when the offered durations change; unflagged
        // repeats and the release after the re-arm are not an answer.
        probe.add("Space pressed on Allow while armed ...", { row: true, steps: ["tab:Allow", "press:Space"] });
        probe.add("... unflagged Space repeat and release after a duration re-arm", { rearm: true,
                  steps: ["press:Space", "none", "press:Space", "release:Space", "none", "tab:Allow", "click:Space"],
                  expect: ["{r}:allow:any_host_on_domain:this_time:true"] });
        // A key that answered, whose release never reached the window (it went
        // to another one after the alert hid): the next prompt's FIRST press
        // answers (#258 re-review M1).
        for (const k of ["Escape", "Return", "Enter"]) {
            probe.add(k + " answers and is never released ...", { row: true, steps: ["press:" + k], expect: [k === "Escape" ? D : A] });
            probe.add("... one fresh " + k + " on the prompt after the alert hid", { row: true, hide: true,
                      steps: ["click:" + k], expect: [k === "Escape" ? D : A] });
        }
        // The same key still physically held when the alert comes back repeats
        // there, unflagged, and is swallowed again until it is released.
        for (const k of ["Return", "Enter"]) {
            probe.add(k + " answers and stays down ...", { row: true, steps: ["tab:Allow", "press:" + k], expect: [A] });
            probe.add("... repeats on the alert that comes back are swallowed (" + k + ")", { row: true, hide: true,
                      pre: () => { alert.keyCatcher.forceActiveFocus(); tc.keyPress(probe.keyOf[k]); tc.keyPress(probe.keyOf[k]); },
                      steps: ["press:" + k, "none", "release:" + k, "none", "click:" + k], expect: [A] });
        }
        // The window is deactivated and activated again (focus stolen): the
        // delay starts over, and a key down across it repeats during the delay.
        for (const k of ["Return", "Enter"]) {
            probe.add("an armed alert (" + k + ")", { row: true });
            probe.add(k + " across a deactivate / activate of the same armed alert", {
                      pre: () => {
                          alert.windowActiveChanged(false);
                          alert.windowActiveChanged(true);
                          probe.expect(!alert.armed, "activating did not start the delay again");
                          alert.keyCatcher.forceActiveFocus();
                          tc.keyPress(probe.keyOf[k]);
                      },
                      steps: ["press:" + k, "none", "release:" + k, "none", "click:" + k], expect: [A] });
        }
        // Focus on a duration the re-arm removes stays on the choice, so Return
        // selects (it does not answer); focus on Allow stays on Allow (M2).
        probe.add("Forever focused ...", { row: true, steps: ["tab:Forever"] });
        probe.add("... the durations shrink to Once", { rearm: true,
                  run: () => probe.expect(probe.where(probe.focused()) === "Once", "focus after the durations shrank: " + probe.where(probe.focused())),
                  steps: ["click:Return", "none"] });
        probe.add("Allow focused ...", { row: true, steps: ["tab:Allow"] });
        probe.add("... a re-arm keeps it there", { rearm: true,
                  run: () => probe.expect(probe.where(probe.focused()) === "Allow", "focus after a re-arm: " + probe.where(probe.focused())),
                  steps: ["click:Return"], expect: ["{r}:allow:any_host_on_domain:this_time:true"] });
        // A replacement row starts with the focus off Deny.
        probe.add("Deny focused, never answered ...", { row: true, steps: ["tab:Deny"] });
        probe.add("... Return on the replacement row is Allow", { row: true, steps: ["click:Return"], expect: [A] });
        probe.add("a repeated Space / Return press is not an answer", { row: true, run: probe.repeats });
    }
    // The native-button guard, with the auto-repeat flag QtTest cannot set.
    function repeats() {
        const ev = (key, rep) => ({ key: key, isAutoRepeat: rep, accepted: false });
        probe.expect(alert.armed, "repeats: not armed");
        const space = ev(Qt.Key_Space, true);
        alert.verdictButtonPressed(space, "allow");
        probe.expect(space.accepted === true, "an auto-repeated Space reached the button");
        const enter = ev(Qt.Key_Return, true);
        alert.verdictButtonPressed(enter, "allow");
        probe.expect(enter.accepted === true, "an auto-repeated Return was not swallowed");
        probe.expect(feedStub.submitted.length === 0, "a repeat sent " + JSON.stringify(feedStub.submitted));
        alert.keyReleased(Qt.Key_Space);
        alert.keyReleased(Qt.Key_Return);
        const fresh = ev(Qt.Key_Space, false);
        alert.verdictButtonPressed(fresh, "allow");
        probe.expect(fresh.accepted === false, "a fresh Space was not left to the button");
    }
    // Names, initial focus, the full Tab / Shift+Tab chain.
    function structure() {
        probe.expect(probe.focused() === alert.keyCatcher, "initial focus: " + probe.where(probe.focused()));
        probe.walk(alert.contentItem, "alert");
        const wanted = ["Applies to: Any host", "Applies to: Any host on this domain", "Applies to: This host only",
                        "For how long: Once", "For how long: 5 minutes", "For how long: Until firewall restarts",
                        "For how long: Forever", "Allow", "Deny"];
        for (const n of wanted) probe.expect(probe.names[n] === true, "no control named '" + n + "': " + JSON.stringify(Object.keys(probe.names)));
        probe.expect(probe.checked === wanted.length, "controls checked: " + probe.checked);
        const bad = Object.keys(probe.missing);
        probe.expect(bad.length === 0, "unnamed: " + bad.join("; "));
        const seen = [];
        for (let i = 0; i < probe.order.length; ++i) { tc.keyClick(Qt.Key_Tab); seen.push(probe.where(probe.focused())); }
        probe.expect(JSON.stringify(seen) === JSON.stringify(probe.order), "Tab order: " + JSON.stringify(seen));
        tc.keyClick(Qt.Key_Tab);
        probe.expect(probe.where(probe.focused()) === "Any host", "Tab after Deny: " + probe.where(probe.focused()));
        tc.keyClick(Qt.Key_Backtab);
        probe.expect(probe.where(probe.focused()) === "Deny", "Shift+Tab from the first: " + probe.where(probe.focused()));
        const back = [];
        for (let i = 0; i < probe.order.length; ++i) { tc.keyClick(Qt.Key_Backtab); back.push(probe.where(probe.focused())); }
        probe.expect(JSON.stringify(back) === JSON.stringify(probe.order.slice().reverse().slice(1).concat(["Deny"])),
                     "Shift+Tab order: " + JSON.stringify(back));
    }
    // "" once the alert is armed on the stage's row and its controls are still.
    function ready() {
        if (!alert.armed || !alert.visible || alert.rowId !== probe.current) return "the armed alert on " + probe.current;
        for (const [k, c] of [["allow", alert.allowButton], ["deny", alert.denyButton]]) {
            if (!c.enabled) return "the " + k + " button enabled";
            if (!probe.stable(k, c)) return "the " + k + " button to stop moving";
        }
        return "";
    }
    function start(s, n) {
        feedStub.submitted = [];
        if (s.row) {
            feedStub.appBound = true;
            const previous = probe.current;
            probe.current = "r" + n;
            const insert = () => connModel.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [probe.row(probe.current)] }));
            if (s.hide && previous !== "") {
                // Nothing pending: the alert hides, then the next row shows it.
                probe.decide(previous);
                alert.refresh();
                probe.expect(!alert.visible, s.name + ": the alert did not hide");
                insert();
            } else {
                // The next row is already queued when the old one resolves, so
                // the alert never hides: a plain row replacement.
                insert();
                if (previous !== "") probe.decide(previous);
            }
            alert.refresh();
        }
        if (s.rearm) {
            // A bridge without app-bound rules offers only "Once" for a host scope.
            feedStub.appBound = false;
            alert.refresh();
            alert.scopeSelector.currentToken = "any_host_on_domain";
        }
        if (s.pre) s.pre();
    }
    function decide(id) {
        const r = probe.row(id);
        r.action = "allow";
        connModel.applyServerMessageJson(JSON.stringify({ action: "updateConnectionRows", rows: [r] }));
    }
    function execute(s) {
        probe.expect(feedStub.submitted.length === 0, s.name + ": sent before any key: " + JSON.stringify(feedStub.submitted));
        if (s.run) s.run();
        for (const st of s.steps) probe.step(s.name, st);
        const want = s.expect.map(e => e.replace("{r}", probe.current));
        probe.expect(JSON.stringify(feedStub.submitted) === JSON.stringify(want),
                     s.name + ": sent " + JSON.stringify(feedStub.submitted) + ", wanted " + JSON.stringify(want));
        if (s.state) probe.expect(alert.scopeToken + "/" + alert.durationToken === s.state,
                                  s.name + ": state " + alert.scopeToken + "/" + alert.durationToken + ", wanted " + s.state);
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            if (probe.inTick) return;
            probe.inTick = true;
            let done = false;
            try {
                const s = probe.stages[(probe.phase - 1) >> 1];
                const executing = probe.phase > 0 && (probe.phase % 2) === 0;
                const waiting = executing ? probe.ready() : "";
                if (waiting !== "") {
                    if (++probe.stalls < probe.maxStalls) return;
                    throw new Error("phase " + probe.phase + " (" + s.name + ") never ready for " + waiting);
                }
                probe.stalls = 0;
                if (probe.phase === 0) {
                    probe.build();
                    alert.requestActivate();
                } else if (!executing) {
                    probe.start(s, (probe.phase + 1) >> 1);
                } else {
                    probe.execute(s);
                    done = probe.phase >= probe.stages.length * 2;
                }
                probe.phase++;
            } catch (e) {
                probe.failures.push("phase " + probe.phase + " threw: " + e);
                done = true;
            } finally {
                probe.inTick = false;
            }
            if (done) {
                stop();
                try {
                    if (probe.failures.length > 0) {
                        throw new Error("alert keyboard probe: " + probe.failures.join("; "));
                    }
                    console.warn("ALERT_KEYBOARD_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

#[test]
fn the_alert_works_by_keyboard_alone() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
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
        "alert keyboard probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
