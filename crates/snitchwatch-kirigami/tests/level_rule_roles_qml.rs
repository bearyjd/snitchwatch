//! The tree headers' `levelRuleState` / `levelRuleName` roles follow the
//! firewall's rule list with `dataChanged` for those two roles only, never a
//! model reset or row insert/remove; and the rule button invokables refuse
//! with a reason (no bridge here, no confirmation, no program path) instead
//! of sending. Run headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/level_rule_roles_probe.qml";
const DONE_MARKER: &str = "LEVEL_RULE_ROLES_PROBE_DONE";

#[test]
fn level_rule_roles_follow_rules_without_a_reset() {
    init_headless_qt_env();
    // Demo mode must be off for the "inert" assertion below.
    std::env::remove_var("SNITCHWATCH_DEMO");

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 400
    height: 300
    property var failures: []
    property int phase: 0
    property int resets: 0
    property int structural: 0
    property var changes: []
    readonly property int stateRole: 32
    readonly property int nameRole: 33
    readonly property string prog: "/usr/bin/curl"

    ConnectionsModel { id: model }

    Connections {
        target: model
        function onModelReset() { probe.resets++; }
        function onRowsInserted() { probe.structural++; }
        function onRowsRemoved() { probe.structural++; }
        function onDataChanged(top, bottom, roles) {
            probe.changes.push(top.row + "-" + bottom.row + ":" + roles.join(","));
        }
    }

    function row(id, host, path) {
        return { id: id, process: "curl", processPath: path, dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function hostRule(name, action, host) {
        return { name: name, enabled: true, action: action, duration: "always",
                 operator: { type: "list", operands: [
                     { type: "simple", operand: "process.path", data: probe.prog, sensitive: true },
                     { type: "simple", operand: "dest.host", data: host, sensitive: false } ] } };
    }
    function setRules(rules) {
        model.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: rules }));
    }
    function stateAt(r) { return model.data(model.index(r, 0), probe.stateRole); }
    function nameAt(r) { return model.data(model.index(r, 0), probe.nameRole); }
    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function mark() { probe.resets = 0; probe.structural = 0; probe.changes = []; }

    Timer {
        interval: 50
        running: true
        repeat: true
        onTriggered: {
            let done = false;
            try {
                if (probe.phase === 0) {
                    model.applyServerMessageJson(JSON.stringify({
                        action: "insertConnectionRows",
                        rows: [probe.row("a", "www.example.com", probe.prog)]
                    }));
                } else if (probe.phase === 1) {
                    model.setPendingOnly(false);
                    model.toggleProcessGroup(probe.prog);
                    model.toggleDomainGroup(probe.prog, "example.com");
                    probe.expect(model.count === 3, "program, domain, host headers: " + model.count);
                    for (let r = 0; r < 3; ++r)
                        probe.expect(probe.stateAt(r) === "none", "row " + r + " starts none: " + probe.stateAt(r));
                    probe.mark();
                    probe.setRules([probe.hostRule("r-host", "deny", "WWW.example.com")]);
                    probe.expect(probe.resets === 0 && probe.structural === 0,
                                 "a rule change reset or moved rows");
                    probe.expect(probe.changes.length === 1 && probe.changes[0] === "0-2:32,33,34",
                                 "one dataChanged over the headers for the three roles: " + probe.changes);
                    probe.expect(probe.stateAt(0) === "partial", "program: narrower rule: " + probe.stateAt(0));
                    probe.expect(probe.stateAt(1) === "partial", "domain: narrower rule: " + probe.stateAt(1));
                    probe.expect(probe.stateAt(2) === "deny", "host: deny: " + probe.stateAt(2));
                    probe.expect(probe.nameAt(2) === "r-host", "host rule name: " + probe.nameAt(2));
                    probe.expect(probe.nameAt(0) === "", "partial has no single rule to delete");
                    probe.mark();
                    probe.setRules([probe.hostRule("r-host", "allow", "www.example.com")]);
                    probe.expect(probe.stateAt(2) === "allow", "host: allow: " + probe.stateAt(2));
                    probe.mark();
                    probe.setRules([]);
                    probe.expect(probe.resets === 0 && probe.structural === 0, "clearing reset");
                    probe.expect(probe.stateAt(2) === "none" && probe.nameAt(2) === "",
                                 "back to none: " + probe.stateAt(2));
                } else if (probe.phase === 2) {
                    probe.mark();
                    probe.setRules([]);
                    probe.expect(probe.changes.length === 0, "an unchanged list repainted: " + probe.changes);
                    // Invokables refuse with a reason, and change nothing.
                    const noBridge = model.createLevelRule(2, probe.prog, "www.example.com", "deny", true);
                    probe.expect(noBridge !== "", "no bridge: must say why");
                    probe.expect(probe.stateAt(2) === "none", "a refused create left pending: " + probe.stateAt(2));
                    probe.expect(model.createLevelRule(0, probe.prog, "", "deny", false).indexOf("confirm") >= 0,
                                 "program level needs confirming");
                    probe.expect(model.createLevelRule(2, "curl", "www.example.com", "deny", true) !== "",
                                 "unbindable program refused");
                    probe.expect(model.createLevelRule(7, probe.prog, "x", "deny", true) !== "", "unknown level refused");
                    probe.expect(model.deleteLevelRule(2, probe.prog, "www.example.com") !== "", "nothing to delete");
                    probe.expect(probe.resets === 0 && probe.structural === 0, "invokables reset the model");
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
                        throw new Error("level rule roles probe: " + probe.failures.join("; "));
                    }
                    console.warn("LEVEL_RULE_ROLES_PROBE_DONE");
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
    let bad: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains(PROBE_URL))
        .filter(|l| !l.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "level rule roles probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
