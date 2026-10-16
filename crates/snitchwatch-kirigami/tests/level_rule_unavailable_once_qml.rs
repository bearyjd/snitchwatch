//! `unavailable` for a node with no rule when the Settings default duration is
//! "this time" (a rule can't be added once-only); an existing rule still shows
//! allow/deny. Settings are read from a scratch `XDG_CONFIG_HOME`. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/level_rule_unavailable_once_probe.qml";
const DONE_MARKER: &str = "LEVEL_RULE_UNAVAILABLE_ONCE_PROBE_DONE";

#[test]
fn level_rule_state_is_unavailable_when_the_default_duration_is_once() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");
    // The Settings the model reads (default duration), in a scratch dir.
    let config = tempfile::tempdir().expect("scratch config dir");
    std::fs::create_dir_all(config.path().join("snitchwatch")).expect("settings dir");
    std::fs::write(
        config.path().join("snitchwatch/settings.json"),
        r#"{"default_duration":"this_time"}"#,
    )
    .expect("settings file");
    std::env::set_var("XDG_CONFIG_HOME", config.path());
    // The probe rewrites that file (Settings changing under the model).
    std::env::set_var("QML_XHR_ALLOW_FILE_WRITE", "1");
    let settings_url = format!(
        "file://{}/snitchwatch/settings.json",
        config.path().display()
    );

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
    readonly property int levelRole: 30
    readonly property int keyRole: 10
    readonly property int stateRole: 32
    readonly property int reasonRole: 34

    ConnectionsModel { id: model }
    // Repaints of the rule roles (a Settings change must cause exactly these).
    property int repaints: 0
    property var lastRoles: []
    Connections {
        target: model
        function onDataChanged(top, bottom, roles) { probe.repaints++; probe.lastRoles = roles; }
    }
    // Rewrites the Settings file; the write finishes before the next tick.
    // The probe fails if the write didn't land (#116): the file is read back.
    function writeSettings(body) {
        const x = new XMLHttpRequest();
        x.open("PUT", "__SETTINGS_URL__");
        x.onreadystatechange = function () {
            if (x.readyState !== XMLHttpRequest.DONE) return;
            probe.wrote = x.status + ":" + x.responseText;
            const back = new XMLHttpRequest();
            back.open("GET", "__SETTINGS_URL__");
            back.onreadystatechange = function () {
                if (back.readyState === XMLHttpRequest.DONE && back.responseText !== body)
                    probe.failures.push("the settings write didn't land (" + probe.wrote + "): read back '" + back.responseText + "'");
            };
            back.send();
        };
        x.send(body);
    }
    property string wrote: ""

    function row(id, process, path, host) {
        return { id: id, process: process, processPath: path, dstHost: host,
                 dstIp: "", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function at(r, role) { return model.data(model.index(r, 0), role); }
    // "state/reason" of the header (level, key), or "missing".
    function find(level, key) {
        for (let r = 0; r < model.count; ++r) {
            if (probe.at(r, probe.levelRole) === level && probe.at(r, probe.keyRole) === key)
                return { state: probe.at(r, probe.stateRole), reason: probe.at(r, probe.reasonRole) };
        }
        return null;
    }
    function dump() {
        const out = [];
        for (let r = 0; r < model.count; ++r)
            out.push(probe.at(r, probe.levelRole) + ":" + probe.at(r, probe.keyRole) + "=" + probe.at(r, probe.stateRole));
        return out.join(" ");
    }

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
                        rows: [
                            probe.row("a", "curl", "/usr/bin/curl", "www.example.com"),
                            probe.row("b", "curl", "/usr/bin/curl", "bucket.s3.amazonaws.com"),
                            probe.row("c", "curl", "/usr/bin/curl", "ec2.amazonaws.com"),
                            probe.row("d", "wget", "/usr/bin/wget", "co.uk"),
                            probe.row("e", "weird", "", "www.example.com")
                        ]
                    }));
                } else if (probe.phase === 1) {
                    model.setPendingOnly(false);
                    for (const p of ["/usr/bin/curl", "/usr/bin/wget", "weird"])
                        model.toggleProcessGroup(p);
                } else if (probe.phase === 2) {
                    model.toggleDomainGroup("/usr/bin/curl", "amazonaws.com");
                    model.toggleDomainGroup("/usr/bin/wget", "co.uk");
                    model.toggleDomainGroup("weird", "example.com");
                    model.toggleDomainGroup("/usr/bin/curl", "example.com");
                } else if (probe.phase === 3) {
                    const d = probe.dump();
                    // The default duration is once-only: a node with no rule
                    // can't get one, whatever the node is.
                    for (const [level, key] of [[0, "/usr/bin/curl"], [1, "example.com"], [0, "/usr/bin/wget"]]) {
                        const f = probe.find(level, key);
                        probe.expect(f !== null && f.state === "unavailable", level + ":" + key + " in " + d);
                        probe.expect(f !== null && f.reason.indexOf("default duration") >= 0, "reason: " + (f ? f.reason : "?"));
                    }
                    // Creating one is refused with the same reason.
                    const why = model.createLevelRule(1, "/usr/bin/curl", "example.com", "allow", true, "");
                    probe.expect(why.indexOf("default duration") >= 0, "create: " + why);
                    // A program-wide rule still needs confirming first.
                    probe.expect(model.createLevelRule(0, "/usr/bin/curl", "", "allow", false, "").indexOf("needs confirming") >= 0,
                                 "unconfirmed program rule");
                    // An existing rule at the level still shows, so it can be removed.
                    model.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: [
                        { name: "r-host", enabled: true, action: "allow", duration: "always",
                          operator: { type: "list", operands: [
                              { type: "simple", operand: "process.path", data: "/usr/bin/curl", sensitive: true },
                              { type: "simple", operand: "dest.host", data: "www.example.com", sensitive: false } ] } } ] }));
                    const h = probe.find(2, "www.example.com");
                    probe.expect(h !== null && h.state === "allow" && h.reason === "", "existing rule: " + JSON.stringify(h));
                    // A duration the user was shown that is no longer Settings'
                    // is refused, before anything is sent.
                    const stale = model.createLevelRule(1, "/usr/bin/curl", "example.com", "allow", true, "forever");
                    probe.expect(stale.indexOf("changed") >= 0, "stale duration: " + stale);
                    // Settings change under the model (checked next tick, once
                    // the write has landed).
                    probe.writeSettings('{"default_duration":"forever"}');
                } else if (probe.phase === 4) {
                    // The file alone changes nothing: states come from the cached
                    // token, not a read per data() call. The model's refresh
                    // applies it, and repaints only the rule roles.
                    const before = probe.find(0, "/usr/bin/wget");
                    probe.expect(before !== null && before.state === "unavailable", "cached token: " + JSON.stringify(before) + " wrote " + probe.wrote);
                    probe.expect(model.levelRuleDuration() === "this_time", "duration re-read per call: " + model.levelRuleDuration());
                    const n = probe.repaints;
                    model.refreshLevelRuleDuration();
                    const after = probe.find(0, "/usr/bin/wget");
                    probe.expect(after !== null && after.state === "none", "after refresh: " + JSON.stringify(after));
                    probe.expect(model.levelRuleDuration() === "forever", "duration after refresh: " + model.levelRuleDuration());
                    probe.expect(probe.repaints === n + 1, "repaints: " + (probe.repaints - n));
                    probe.expect(probe.lastRoles.length === 3 && probe.lastRoles.indexOf(probe.stateRole) >= 0,
                                 "repainted roles: " + JSON.stringify(probe.lastRoles));
                    // An unchanged duration repaints nothing.
                    model.refreshLevelRuleDuration();
                    probe.expect(probe.repaints === n + 1, "repainted without a change");
                    // And back to once-only.
                    probe.writeSettings('{"default_duration":"this_time"}');
                } else if (probe.phase === 5) {
                    model.refreshLevelRuleDuration();
                    const back = probe.find(0, "/usr/bin/wget");
                    probe.expect(back !== null && back.state === "unavailable", "after switching back: " + JSON.stringify(back));
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
                        throw new Error("unavailable probe: " + probe.failures.join("; "));
                    }
                    console.warn("LEVEL_RULE_UNAVAILABLE_ONCE_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
"#;

    let qml = qml.replace("__SETTINGS_URL__", &settings_url);

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
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
        .filter(|l| l.contains(PROBE_URL))
        .filter(|l| !l.contains(DONE_MARKER))
        .collect();
    assert!(
        bad.is_empty(),
        "unavailable probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
