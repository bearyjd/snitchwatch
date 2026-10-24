//! `LevelRuleActions.qml`: a program-wide rule opens a confirm naming the
//! scope, program and duration and sends nothing until accepted; domain and
//! host rules need no confirm; the outcome shown is the model's
//! `levelRuleFinished`, a refusal's reason is shown, and the 1 s poll timer
//! runs only while a command waits. The last phase drives the real page and
//! model, which refuse (no firewall connected). Messages start with the label
//! of their node and one node's never clears another's; the dialog names the
//! program file and the duration the model will use; a keyboard accept on the
//! menu sends nothing before the dialog's accept; session-bound confirmations
//! survive ordinary resets and refuse replacement sessions. Run headless with
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
    property int settle: 0
    property var pooled: null
    property bool geometryChecked: false

    // Records every call; answers like a model that queued the command.
    QtObject {
        id: stub
        property var calls: []
        property int polls: 0
        property string answer: ""
        property string session: "1:"
        signal modelReset()
        function currentSourceSession() { return session; }
        signal levelRuleFinished(int level, string processKey, string key, int ending, string status)
        function createLevelRule(level, p, k, choice, confirmed, shown) {
            calls.push(["create", level, p, k, choice, confirmed, shown].join("|"));
            return answer;
        }
        function deleteLevelRule(level, p, k) {
            calls.push(["delete", level, p, k].join("|"));
            return answer;
        }
        function pollLevelRules() { polls++; }
        // Not the QML default ("forever"): the dialog must ask the model.
        function levelRuleDuration() { return "for_5_minutes"; }
    }
    LevelRuleActions { id: actions; model: stub; width: 600 }
    // A real button wired like the row wires it.
    RuleButton {
        id: keyBtn
        x: 650
        z: 100
        level: 0
        program: "/usr/bin/k"
        nodeLabel: "k"
        onCreateRequested: (level, program, key, label, choice) => actions.create(level, program, key, label, choice)
    }

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
            // Let the closing confirm dialog finish before clicking the page.
            if (probe.phase === 6 && ++probe.settle < 6) {
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
                                 && text.indexOf("The rule lasts: 5 minutes.") >= 0
                                 && text.indexOf(Durations.labelFor(Durations.defaultToken)) < 0
                                 && text.indexOf("Deny") >= 0, "confirm text: " + text);
                    // It names the program file as plain text, not just the process.
                    probe.expect(actions.confirmProgramLabel.text.indexOf("/usr/bin/p") >= 0
                                 && actions.confirmProgramLabel.textFormat === Text.PlainText
                                 && actions.confirmBodyLabel.textFormat === Text.PlainText,
                                 "dialog program file: " + actions.confirmProgramLabel.text);
                    // Cancelling sends nothing and forgets the request (below,
                    // once the open dialog has been laid out and measured).
                } else if (probe.phase === 1) {
                    if (!probe.geometryChecked) {
                        probe.geometryChecked = true;
                        // Laid out, not stacked: both labels have a width and the
                        // program file sits below the text, not on top of it.
                        const body = actions.confirmBodyLabel, file = actions.confirmProgramLabel;
                        probe.expect(body.width > 0 && file.width > 0, "dialog labels have no width: " + body.width + "," + file.width);
                        probe.expect(file.mapToItem(null, 0, 0).y >= body.mapToItem(null, 0, 0).y + body.height,
                                     "dialog labels overlap: body " + body.mapToItem(null, 0, 0).y + "+" + body.height
                                     + " file " + file.mapToItem(null, 0, 0).y);
                        actions.confirmDialog.close();
                        return;
                    }
                    probe.expect(!actions.confirmDialog.opened, "confirm still open after cancel");
                    probe.expect(stub.calls.length === 0, "sent after cancel: " + stub.calls.join(","));
                    probe.expect(!actions.pollTimer.running, "timer running after cancel");
                    // Accepting sends once, confirmed.
                    actions.create(0, "/usr/bin/p", "", "p", "allow");
                    actions.confirmDialog.customFooterActions[0].trigger();
                    probe.expect(stub.calls.join(",") === "create|0|/usr/bin/p||allow|true|for_5_minutes", "accepted call: " + stub.calls.join(","));
                    probe.expect(actions.pollTimer.running, "timer off with a command waiting");
                } else if (probe.phase === 2) {
                    probe.expect(stub.polls >= 1, "poll never called while waiting: " + stub.polls);
                    // The outcome comes from the signal.
                    stub.levelRuleFinished(0, "/usr/bin/p", "", 1, "The rule was created.");
                    probe.expect(actions.message === "p: The rule was created.", "message: " + actions.message);
                    probe.expect(!actions.pollTimer.running, "timer still running after the answer");
                    probe.expect(actions.visible, "message bar hidden");
                    // Domain level: straight out, no confirm, confirmed=false.
                    stub.calls = [];
                    actions.create(1, "/usr/bin/p", "example.com", "example.com", "deny");
                    probe.expect(!actions.confirmDialog.opened, "domain level opened a confirm");
                    probe.expect(stub.calls.join(",") === "create|1|/usr/bin/p|example.com|deny|false|", "domain call: " + stub.calls.join(","));
                    probe.expect(actions.pollTimer.running, "timer off for a domain command");
                    // Two in flight: the timer runs until both end.
                    actions.remove(2, "/usr/bin/p", "a.example.com", "a.example.com");
                    stub.levelRuleFinished(1, "/usr/bin/p", "example.com", 0, "The rule wasn't created: nope");
                    probe.expect(actions.pollTimer.running, "timer off with one still waiting");
                    stub.levelRuleFinished(2, "/usr/bin/p", "a.example.com", 2, "No answer");
                    probe.expect(!actions.pollTimer.running, "timer on with nothing waiting");
                    // A refusal shows its reason and starts no wait.
                    stub.answer = "A rule can't be made for this row.";
                    actions.create(1, "/usr/bin/p", "x.com", "x.com", "allow");
                    probe.expect(actions.message.indexOf("x.com: " + stub.answer) >= 0, "refusal not shown: " + actions.message);
                    probe.expect(!actions.pollTimer.running, "timer on after a refusal");
                    // The bar's text is the one on screen.
                    probe.expect(actions.children.length > 0, "no bar");
                    // Two commands at once: each answer keeps its own label,
                    // and a new send clears neither.
                    stub.answer = "";
                    actions.dismiss();
                    actions.create(1, "/usr/bin/p", "one.com", "one.com", "allow");
                    actions.create(2, "/usr/bin/p", "b.two.com", "b.two.com", "deny");
                    probe.expect(actions.message === "", "a send cleared the bar: " + actions.message);
                    stub.levelRuleFinished(2, "/usr/bin/p", "b.two.com", 1, "The rule was created.");
                    probe.expect(actions.message === "b.two.com: The rule was created.", "first answer: " + actions.message);
                    actions.create(1, "/usr/bin/p", "three.com", "three.com", "allow");
                    probe.expect(actions.message === "b.two.com: The rule was created.", "a send cleared another row's message: " + actions.message);
                    stub.levelRuleFinished(1, "/usr/bin/p", "one.com", 0, "The rule wasn't created: nope");
                    probe.expect(actions.message === "b.two.com: The rule was created.\none.com: The rule wasn't created: nope",
                                 "two messages: " + actions.message);
                    stub.levelRuleFinished(1, "/usr/bin/p", "three.com", 2, "No answer");
                    actions.dismiss();
                    probe.expect(actions.message === "" && !actions.visible, "dismiss left a message");
                    // A keyboard accept on the menu item is only a request: a
                    // program-wide rule still waits for the dialog's accept.
                    stub.calls = [];
                    keyBtn.ruleState = "none";
                    tc.mouseClick(keyBtn.button);
                    // Real page: expand to a domain header and ask for a rule
                    // with no firewall connected.
                    realModel.setGroupedMode(true);
                    realModel.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "x.example.com")]
                    }));
                } else if (probe.phase === 3) {
                    probe.expect(keyBtn.chooseMenu.visible, "keyboard button: menu did not open");
                    keyBtn.chooseMenu.currentIndex = 0;
                    tc.keyClick(Qt.Key_Return);
                } else if (probe.phase === 4) {
                    probe.expect(stub.calls.length === 0, "keyboard accept sent without the dialog: " + stub.calls.join(","));
                    probe.expect(actions.confirmDialog.visible, "keyboard accept did not ask first");
                    actions.confirmDialog.customFooterActions[0].trigger();
                    probe.expect(stub.calls.join(",") === "create|0|/usr/bin/k||allow|true|for_5_minutes", "dialog accept call: " + stub.calls.join(","));
                    stub.levelRuleFinished(0, "/usr/bin/k", "", 1, "ok");
                } else if (probe.phase === 5) {
                    realModel.setPendingOnly(false);
                } else if (probe.phase === 6) {
                    const list = probe.findList(page);
                    tc.mouseClick(list.itemAtIndex(0), 2, 2);
                } else if (probe.phase === 7) {
                    const list = probe.findList(page);
                    const dom = list.itemAtIndex(1);
                    if (dom === null || dom.groupLevel !== 1) {
                        probe.failures.push("list count " + list.count + " model count " + realModel.count + " idx0=" + (list.itemAtIndex(0) ? list.itemAtIndex(0).groupKey : "null") + " cur=" + list.currentIndex);
                    }
                    probe.expect(dom !== null && dom.groupLevel === 1, "no domain header");
                    probe.expect(dom.ruleButton.visible && dom.ruleButton.ruleState === "none", "domain state: " + dom.ruleButton.ruleState);
                    probe.expect(list.itemAtIndex(0).ruleButton.visible, "program button hidden");
                    page.levelRules.create(1, dom.ruleProgram, dom.ruleKey, dom.groupLabel, "allow");
                    probe.expect(page.levelRules.message !== "", "no reason shown for a refusal");
                    probe.expect(!page.levelRules.pollTimer.running, "timer on after the model refused");
                    probe.expect(page.levelRules.message.indexOf("connected") >= 0, "reason: " + page.levelRules.message);
                    // A real model refuses a program-wide rule that isn't
                    // confirmed, before anything else.
                    const need = realModel.createLevelRule(0, dom.ruleProgram, "", "allow", false, "");
                    probe.expect(need === "A rule for every connection of a program needs confirming.", "unconfirmed program rule: " + need);
                    // Open the domain row's menu, then let the list pool it.
                    probe.pooled = dom;
                    tc.mouseClick(dom.ruleButton.button);
                } else if (probe.phase === 8) {
                    probe.expect(page.connectionActions.menu.visible, "row menu did not open");
                    tc.mouseClick(probe.findList(page).itemAtIndex(0), 2, 2);
                } else if (probe.phase === 9) {
                    probe.expect(probe.findList(page).count === 1, "group did not collapse: " + probe.findList(page).count);
                    // Clicking outside the popup dismisses it before collapsing.
                    probe.expect(!page.connectionActions.menu.visible,
                                 "outside click kept the page menu open");
                    const sent = stub.calls.length;
                    actions.create(0, "/usr/bin/p", "", "p", "allow", "1:");
                    probe.expect(actions.confirmDialog.visible, "session-bound confirm did not open");
                    stub.modelReset();
                    probe.expect(actions.asked !== null, "ordinary reset dismissed rule confirmation");
                    stub.session = "2:";
                    stub.modelReset();
                    probe.expect(actions.asked === null, "replacement session retained old confirmation");
                    actions.accepted();
                    probe.expect(stub.calls.length === sent, "old confirmation sent to replacement service");
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
