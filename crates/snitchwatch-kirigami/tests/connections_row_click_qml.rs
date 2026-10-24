//! A click on a leaf connection row must open the inspector for that row and
//! make it the list's current item. The row's click handler lives in a child
//! `MouseArea`, where the `ListView` attached properties do not exist (they
//! are only defined on the delegate root); an unqualified `ListView.view` there
//! is null, the handler throws, and `openInspector` never runs. Drives the real
//! `ConnectionsPage` + real `ConnectionRow` delegate with a QtTest `mouseClick`.
//! A throw inside `ConnectionRow.qml` is reported against that file's URL, not
//! the probe's, so the Rust side also fails on any TypeError in the capture.
//! Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/connections_row_click_probe.qml";
const DONE_MARKER: &str = "ROW_CLICK_PROBE_DONE";

#[test]
fn clicking_a_leaf_row_opens_the_inspector_and_selects_it() {
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
    width: 1400
    height: 900
    property var failures: []
    property int phase: 0
    property var menuRow: null
    readonly property var liveActions: page.connectionActions

    QtObject {
        id: feedStub
        property bool ok: true
        property var sent: []
        function appBoundRulesFor(rowId) { return true; }
        function submitVerdict(rowId, choice, scope, duration, bindableProcessPath) {
            sent = sent.concat([rowId + ":" + choice]);
            return true;
        }
    }
    QtObject {
        id: ruleStub
        property var calls: []
        property var origins: []
        property string currentRule: "rule-A"
        property string session: "1:"
        signal levelRuleFinished(int level, string processKey, string key, int ending, string status)
        function createLevelRule(level, program, key, choice, confirmed, duration) {
            calls = calls.concat([[level, program, key, choice]]);
            return "";
        }
        function createLevelRuleForSession(level, program, key, choice, duration, sourceSession) {
            origins.push(["create", sourceSession]);
            return createLevelRule(level, program, key, choice, level === 0 && duration !== "", duration);
        }
        function deleteLevelRuleForSession(level, program, key, sourceSession, name, state) {
            origins.push(["delete", sourceSession, name, state]);
            if (name !== currentRule) return "The rule changed while you were deciding.";
            return deleteLevelRule(level, program, key);
        }
        function currentSourceSession() { return session; }
        function deleteLevelRule(level, program, key) {
            calls = calls.concat([["remove", level, program, key]]);
            return "";
        }
        function pollLevelRules() {}
        function levelRuleScopeText(level, program, key) { return "Executable " + program + "; " + (level === 0 ? "any host" : key) + "; any port; any protocol"; }
        function levelRuleDuration() { return "forever"; }
    }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        bridgeFeed: feedStub
        model: ConnectionsModel { id: model }
    }
    // An authoritative session change fixture, independent of recycled rows.
    Item {
        id: sessionPage
        property var bridgeFeed: feedStub
        property var levelRules: page.levelRules
        property var model: QtObject {
            id: sessionModel
            property string session: ""
            signal modelReset()
            signal dataChanged()
            signal rowsRemoved()
            function currentSourceSession() { return session; }
        }
    }
    TestCase { id: tc; name: "rowclick"; when: false }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function row(id, host) {
        return { id: id, process: "p", processPath: null, dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function findList(item) {
        if (typeof item.itemAtIndex === "function" && item.currentIndex !== undefined) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) {
            const f = probe.findList(kids[i]);
            if (f) return f;
        }
        return null;
    }
    function header(level, key) {
        const list = page.connectionList;
        for (let i = 0; i < list.count; ++i) {
            const item = list.itemAtIndex(i);
            if (item && item.isGroupHeader && item.groupLevel === level && item.groupKey === key)
                return item;
        }
        throw new Error("missing header " + level + ":" + key);
    }
    function named(item, name) {
        if (item.objectName === name) return item;
        const children = item.children || [];
        for (const child of children) {
            const found = probe.named(child, name);
            if (found) return found;
        }
        return null;
    }
    function pending(id, process, host) {
        const r = probe.row(id, host);
        r.process = process;
        r.processPath = "/usr/bin/" + process;
        r.dstIp = "1.1.1.1";
        r.action = null;
        return r;
    }
    function expectSent(ids, choice) {
        const expected = ids.map(id => id + ":" + choice).sort();
        probe.expect(JSON.stringify(feedStub.sent.slice().sort()) === JSON.stringify(expected),
                     "wrong bulk scope: " + JSON.stringify(feedStub.sent) + "; expected " + JSON.stringify(expected));
        feedStub.sent = [];
    }
    function rightClick(level, key) {
        probe.menuRow = probe.header(level, key);
        const expanded = probe.menuRow.expanded;
        tc.mouseClick(probe.menuRow, 2, 2, Qt.RightButton);
        probe.expect(probe.liveActions.menu.visible, "right click did not open actions");
        probe.expect(probe.menuRow.expanded === expanded, "right click expanded/collapsed group");
    }
    function menuItem(name) {
        for (let i = 0; i < probe.liveActions.menu.count; ++i) {
            const item = probe.liveActions.menu.itemAt(i);
            if (item && item.objectName === name) return item;
        }
        throw new Error("missing menu action " + name);
    }
    function clickPending(allow) {
        const item = probe.menuItem(allow ? "allowPendingAction" : "denyPendingAction");
        tc.mouseClick(item, item.width / 2, item.height / 2);
    }

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    model.setGroupedMode(false);
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "x.example.com"), probe.row("b", "y.example.com")]
                    }));
                } else if (probe.phase === 2) {
                    const list = probe.findList(page);
                    probe.expect(list !== null, "no ListView found under the page");
                    if (list) {
                        probe.expect(list.count === 2, "rows: " + list.count);
                        const item = list.itemAtIndex(1);
                        probe.expect(item !== null, "no delegate at index 1");
                        if (item) {
                            tc.mouseClick(item, 2, 2);
                            probe.expect(page.inspectId === item.rowId,
                                         "inspector did not open for the clicked row: '" + page.inspectId + "'");
                            probe.expect(list.currentIndex === 1,
                                         "currentIndex not set: " + list.currentIndex);
                        }
                    }
                    model.applyServerMessageJson(JSON.stringify({ action: "clearConnectionRows" }));
                    model.setGroupedMode(true);
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        probe.pending("s:www", "app", "www.example.com"),
                        probe.pending("s:api", "app", "api.example.com"),
                        probe.pending("s:other", "app", "other.net"),
                        probe.pending("s:another-app", "another", "www.example.com")
                    ] }));
                } else if (probe.phase === 4) {
                    probe.rightClick(0, "/usr/bin/app");
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        probe.pending("s:new-app", "aaa", "new.net")
                    ] }));
                } else if (probe.phase === 5) {
                    probe.expect(probe.liveActions.menu.visible,
                                 "live traffic reset dismissed open actions menu");
                    probe.clickPending(false);
                    probe.expectSent(["s:www", "s:api", "s:other"], "deny");
                    // Context and inline buttons share the duplicate-submit guard.
                    probe.expect(probe.liveActions.target.program === "/usr/bin/app",
                                 "live update redirected menu to another app");
                    probe.menuRow = probe.header(0, "/usr/bin/app");
                    probe.menuRow.decideOnce("allow", true);
                    probe.expectSent([], "allow");
                    page.inlineVerdicts.submittedIds = ({}); // Stub feed never acknowledges.
                    model.toggleProcessGroup("/usr/bin/app");
                } else if (probe.phase === 7) {
                    probe.menuRow = probe.header(1, "example.com");
                    const handle = probe.named(probe.menuRow, "groupActionsButton");
                    probe.expect(handle !== null, "group Actions handle missing");
                    tc.mouseClick(handle, handle.width / 2, handle.height / 2);
                    probe.expect(probe.liveActions.menu.visible, "Actions handle did not open menu");
                } else if (probe.phase === 8) {
                    probe.clickPending(true);
                    probe.expectSent(["s:www", "s:api"], "allow");
                    page.inlineVerdicts.submittedIds = ({});
                    model.toggleDomainGroup("/usr/bin/app", "example.com");
                } else if (probe.phase === 10) {
                    probe.rightClick(2, "www.example.com");
                } else if (probe.phase === 11) {
                    probe.clickPending(false);
                    probe.expectSent(["s:www"], "deny");
                    page.inlineVerdicts.submittedIds = ({});
                    model.setFilterQuery("www.example.com");
                } else if (probe.phase === 13) {
                    // A filtered header still acts on its complete subtree.
                    const domain = probe.header(1, "example.com");
                    domain.submitted = false; // Stub feed does not apply verdict updates.
                    probe.rightClick(1, "example.com");
                } else if (probe.phase === 14) {
                    probe.clickPending(false);
                    probe.expectSent(["s:www", "s:api"], "deny");
                    page.submitGroupVerdict("/usr/bin/app", 0, "", "", "allow", "old:");
                    probe.expectSent([], "allow");
                    // The target session must be checked at dispatch and on reset,
                    // independently of the old delegate (which can be destroyed).
                    probe.liveActions.page = sessionPage;
                    probe.rightClick(1, "example.com");
                    sessionModel.session = "replacement:";
                    sessionModel.modelReset();
                    probe.liveActions.decide("allow");
                    probe.expectSent([], "allow");
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        probe.pending("s:session-check", "aaa", "new.net")
                    ] }));
                } else if (probe.phase === 15) {
                    probe.expect(!probe.liveActions.menu.visible,
                                 "menu survived an invalid source session");
                    probe.liveActions.page = page;
                    model.setFilterQuery("");
                    page.inlineVerdicts.submittedIds = ({});
                } else if (probe.phase === 17) {
                    // The explicit Actions handle must also survive live resets.
                    probe.menuRow = probe.header(1, "example.com");
                    const handle = probe.named(probe.menuRow, "groupActionsButton");
                    tc.mouseClick(handle, handle.width / 2, handle.height / 2);
                    model.setFilterQuery("other.net");
                    model.applyServerMessageJson(JSON.stringify({ action: "updateConnectionRows", rows: [
                        Object.assign(probe.pending("s:other", "app", "other.net"), { action: "deny" })
                    ] }));
                } else if (probe.phase === 18) {
                    probe.expect(probe.liveActions.menu.visible, "verdict update dismissed Actions popup");
                    probe.clickPending(true);
                    probe.expectSent(["s:www", "s:api"], "allow");
                    page.inlineVerdicts.submittedIds = ({});
                    model.setFilterQuery("");
                } else if (probe.phase === 20) {
                    // A RuleButton routes through the same persistent page owner.
                    const domain = probe.header(1, "example.com");
                    domain.sourceSession = "1:";
                    sessionModel.session = "1:";
                    probe.liveActions.page = sessionPage;
                    tc.mouseClick(domain.ruleButton.button);
                    probe.expect(probe.liveActions.rulesOnly && probe.liveActions.menu.visible,
                                 "RuleButton did not open persistent popup");
                    page.levelRules.model = ruleStub;
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        probe.pending("s:rule-refresh", "zzz", "last.net")
                    ] }));
                } else if (probe.phase === 21) {
                    probe.expect(probe.liveActions.menu.visible, "live reset dismissed RuleButton popup");
                    const item = probe.menuItem("denyGroupRuleAction");
                    tc.mouseClick(item, item.width / 2, item.height / 2);
                    probe.expect(JSON.stringify(ruleStub.calls) === JSON.stringify([[1, "/usr/bin/app", "example.com", "deny"]]),
                                 "RuleButton pick redirected target: " + JSON.stringify(ruleStub.calls));
                } else if (probe.phase === 23) {
                    const domain = probe.header(1, "example.com");
                    domain.levelRuleState = "allow";
                    domain.levelRuleName = "rule-A";
                    domain.sourceSession = "1:";
                    tc.mouseClick(domain.ruleButton.button);
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        probe.pending("s:remove-refresh", "zzz", "last.net")
                    ] }));
                } else if (probe.phase === 24) {
                    probe.expect(probe.liveActions.menu.visible, "live reset dismissed remove-rule popup");
                    probe.expect(probe.liveActions.target.ruleName === "rule-A", "popup did not snapshot exact rule identity");
                    // The same node now represents another rule; keep the captured menu open
                    // and click its actual remove item. The unseen rule must not be deleted.
                    ruleStub.currentRule = "rule-B";
                    probe.header(1, "example.com").levelRuleName = "rule-B";
                    sessionModel.dataChanged();
                    const item = probe.menuItem("removeGroupRuleAction");
                    tc.mouseClick(item, item.width / 2, item.height / 2);
                    probe.expect(ruleStub.calls.length === 1,
                                 "remove-rule pick deleted replacement: " + JSON.stringify(ruleStub.calls));
                    probe.expect(JSON.stringify(ruleStub.origins[1]) === JSON.stringify(["delete", "1:", "rule-A", "allow"]),
                                 "remove-rule lost captured identity/session: " + JSON.stringify(ruleStub.origins));
                    probe.liveActions.page = page;
                    model.setGroupedMode(false);
                } else if (probe.phase === 26) {
                    const leaf = page.connectionList.itemAtIndex(model.visibleRowIndex("s:www"));
                    tc.mouseClick(leaf, 2, 2, Qt.RightButton);
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        probe.pending("s:leaf-refresh", "aaa", "first.net")
                    ] }));
                } else if (probe.phase === 27) {
                    probe.expect(probe.liveActions.menu.visible, "flat insert dismissed leaf popup");
                    probe.clickPending(false);
                    probe.expectSent(["s:www"], "deny");
                } else if (probe.phase === 28) {
                    const leaf = page.connectionList.itemAtIndex(model.visibleRowIndex("s:api"));
                    tc.mouseClick(leaf, 2, 2, Qt.RightButton);
                    model.applyServerMessageJson(JSON.stringify({ action: "updateConnectionRows", rows: [
                        Object.assign(probe.pending("s:api", "app", "api.example.com"), { action: "allow" })
                    ] }));
                } else if (probe.phase === 29) {
                    probe.expect(!probe.liveActions.menu.visible, "answered leaf kept an actionable menu");
                    probe.liveActions.decide("deny");
                    probe.expectSent([], "deny");
                } else if (probe.phase === 30) {
                    // Decided leaves still have useful navigation actions.
                    const leaf = page.connectionList.itemAtIndex(model.visibleRowIndex("s:api"));
                    tc.mouseClick(leaf, 2, 2, Qt.RightButton);
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [
                        probe.pending("s:decided-refresh", "aaa", "first.net")
                    ] }));
                } else if (probe.phase === 31) {
                    probe.expect(probe.liveActions.menu.visible, "traffic dismissed a decided leaf context menu");
                    const item = probe.menuItem("groupConnectionsAction");
                    tc.mouseClick(item, item.width / 2, item.height / 2);
                    probe.expect(model.grouped, "decided leaf Group connections action did nothing");
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
                        throw new Error("row click probe: " + probe.failures.join("; "));
                    }
                    console.warn("ROW_CLICK_PROBE_DONE");
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
        "clicking a leaf row did not open the inspector:\n{}",
        bad_lines.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
