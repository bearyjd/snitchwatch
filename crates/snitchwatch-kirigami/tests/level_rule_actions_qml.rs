//! `LevelRuleActions.qml`: a program-wide rule opens a confirm naming the
//! scope, program and duration and sends nothing until accepted; domain and
//! host rules need no confirm; the outcome shown is the model's
//! `levelRuleFinished`, a refusal's reason is shown, and the 1 s poll timer
//! runs only while a command waits. The last phase drives the real page and
//! model, which refuse (no firewall connected). Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/level_rule_actions_probe.qml";
const DONE_MARKER: &str = "LEVEL_RULE_ACTIONS_PROBE_DONE";

#[test]
fn rule_button_actions_confirm_poll_and_report() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 800
    height: 600
    property var failures: []
    property int phase: 0
    property int ticks: 0

    // Records every call; answers like a model that queued the command.
    QtObject {
        id: stub
        property var calls: []
        property int polls: 0
        property string answer: ""
        signal levelRuleFinished(int level, string processKey, string key, int ending, string status)
        function createLevelRule(level, p, k, choice, confirmed) {
            calls.push(["create", level, p, k, choice, confirmed].join("|"));
            return answer;
        }
        function deleteLevelRule(level, p, k) {
            calls.push(["delete", level, p, k].join("|"));
            return answer;
        }
        function pollLevelRules() { polls++; }
    }
    LevelRuleActions { id: actions; model: stub; width: 600 }

    // The real page and model: refusals come from the Rust side.
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: QtObject { property bool ok: true; function submitVerdict() {} }
        model: ConnectionsModel { id: realModel }
    }
    TestCase { id: tc; name: "ruleactions"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function findList(item) {
        if (typeof item.itemAtIndex === "function" && item.currentIndex !== undefined) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.findList(kids[i]);
            if (f) return f;
        }
        return null;
    }
    function row(id, host) {
        return { id: id, process: "p", processPath: "/usr/bin/p", dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            // Let the 1 s poll fire (phase 2 starts after ~1.4 s).
            if (probe.phase === 2 && ++probe.ticks < 14) {
                return;
            }
            try {
                if (probe.phase === 0) {
                    // Nothing pending: the poll timer is off.
                    probe.expect(!actions.pollTimer.running, "timer running with nothing pending");
                    probe.expect(!actions.visible, "bar visible with no message");
                    // Program level: asks first, sends nothing yet.
                    actions.create(0, "/usr/bin/p", "", "p", "deny");
                    probe.expect(actions.confirmDialog.visible, "program level did not open the confirm");
                    probe.expect(stub.calls.length === 0, "sent before accept: " + stub.calls.join(","));
                    const text = actions.confirmText;
                    probe.expect(text.indexOf("any host") >= 0 && text.indexOf("for p.") >= 0
                                 && text.indexOf(Durations.labelFor(Durations.defaultToken)) >= 0
                                 && text.indexOf("Deny") >= 0, "confirm text: " + text);
                    // Cancelling sends nothing and forgets the request.
                    actions.confirmDialog.close();
                } else if (probe.phase === 1) {
                    probe.expect(!actions.confirmDialog.opened, "confirm still open after cancel");
                    probe.expect(stub.calls.length === 0, "sent after cancel: " + stub.calls.join(","));
                    probe.expect(!actions.pollTimer.running, "timer running after cancel");
                    // Accepting sends once, confirmed.
                    actions.create(0, "/usr/bin/p", "", "p", "allow");
                    actions.confirmDialog.customFooterActions[0].trigger();
                    probe.expect(stub.calls.join(",") === "create|0|/usr/bin/p||allow|true", "accepted call: " + stub.calls.join(","));
                    probe.expect(actions.pollTimer.running, "timer off with a command waiting");
                } else if (probe.phase === 2) {
                    probe.expect(stub.polls >= 1, "poll never called while waiting: " + stub.polls);
                    // The outcome comes from the signal.
                    stub.levelRuleFinished(0, "/usr/bin/p", "", 1, "The rule was created.");
                    probe.expect(actions.message === "The rule was created.", "message: " + actions.message);
                    probe.expect(!actions.pollTimer.running, "timer still running after the answer");
                    probe.expect(actions.visible, "message bar hidden");
                    // Domain level: straight out, no confirm, confirmed=false.
                    stub.calls = [];
                    actions.create(1, "/usr/bin/p", "example.com", "example.com", "deny");
                    probe.expect(!actions.confirmDialog.opened, "domain level opened a confirm");
                    probe.expect(stub.calls.join(",") === "create|1|/usr/bin/p|example.com|deny|false", "domain call: " + stub.calls.join(","));
                    probe.expect(actions.pollTimer.running, "timer off for a domain command");
                    // Two in flight: the timer runs until both end.
                    actions.remove(2, "/usr/bin/p", "a.example.com");
                    stub.levelRuleFinished(1, "/usr/bin/p", "example.com", 0, "The rule wasn't created: nope");
                    probe.expect(actions.pollTimer.running, "timer off with one still waiting");
                    stub.levelRuleFinished(2, "/usr/bin/p", "a.example.com", 2, "No answer");
                    probe.expect(!actions.pollTimer.running, "timer on with nothing waiting");
                    // A refusal shows its reason and starts no wait.
                    stub.answer = "A rule can't be made for this row.";
                    actions.create(1, "/usr/bin/p", "x.com", "x.com", "allow");
                    probe.expect(actions.message === stub.answer, "refusal not shown: " + actions.message);
                    probe.expect(!actions.pollTimer.running, "timer on after a refusal");
                    // The bar's text is the one on screen.
                    probe.expect(actions.children.length > 0, "no bar");
                    // Real page: expand to a domain header and ask for a rule
                    // with no firewall connected.
                    realModel.setGroupedMode(true);
                    realModel.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "x.example.com")]
                    }));
                } else if (probe.phase === 3) {
                    realModel.setPendingOnly(false);
                } else if (probe.phase === 4) {
                    const list = probe.findList(page);
                    tc.mouseClick(list.itemAtIndex(0), 2, 2);
                } else if (probe.phase === 5) {
                    const list = probe.findList(page);
                    const dom = list.itemAtIndex(1);
                    if (dom === null || dom.groupLevel !== 1) {
                        probe.failures.push("list count " + list.count + " model count " + realModel.count);
                    }
                    probe.expect(dom !== null && dom.groupLevel === 1, "no domain header");
                    probe.expect(dom.ruleButton.visible && dom.ruleButton.ruleState === "none", "domain state: " + dom.ruleButton.ruleState);
                    probe.expect(list.itemAtIndex(0).ruleButton.visible, "program button hidden");
                    page.levelRules.create(1, dom.ruleProgram, dom.ruleKey, dom.groupLabel, "allow");
                    probe.expect(page.levelRules.message !== "", "no reason shown for a refusal");
                    probe.expect(!page.levelRules.pollTimer.running, "timer on after the model refused");
                    probe.expect(page.levelRules.message.indexOf("connected") >= 0, "reason: " + page.levelRules.message);
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
                        throw new Error("rule actions probe: " + probe.failures.join("; "));
                    }
                    console.warn("LEVEL_RULE_ACTIONS_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml), &QUrl::from(PROBE_URL));
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

    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("TypeError"))
        .filter(|line| !line.contains(DONE_MARKER))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "the rule button actions did not behave:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
