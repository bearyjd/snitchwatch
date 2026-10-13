//! The tree headers' `levelRuleState` is `unavailable` (with a `levelRuleReason`) for
//! a node with no rule that cannot get one: no absolute program path, a public
//! suffix, a domain over a delegated suffix. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/level_rule_unavailable_probe.qml";
const DONE_MARKER: &str = "LEVEL_RULE_UNAVAILABLE_PROBE_DONE";

#[test]
fn level_rule_state_is_unavailable_when_no_rule_can_be_made() {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");
    // The Settings the model reads (default duration), in a scratch dir.
    let config = tempfile::tempdir().expect("scratch config dir");
    std::fs::create_dir_all(config.path().join("snitchwatch")).expect("settings dir");
    std::fs::write(
        config.path().join("snitchwatch/settings.json"),
        r#"{"default_duration":"forever"}"#,
    )
    .expect("settings file");
    std::env::set_var("XDG_CONFIG_HOME", config.path());

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
                    const want = (level, key, state, reasonPart) => {
                        const f = probe.find(level, key);
                        probe.expect(f !== null && f.state === state, level + ":" + key + " want " + state + " in " + d);
                        probe.expect(f !== null && (reasonPart === "" ? f.reason === "" : f.reason.indexOf(reasonPart) >= 0),
                                     level + ":" + key + " reason " + (f ? f.reason : "?"));
                    };
                    // Bindable, ordinary: a rule can be made.
                    want(0, "/usr/bin/curl", "none", "");
                    want(1, "example.com", "none", "");
                    // A delegated suffix under the domain, and a public suffix.
                    want(1, "amazonaws.com", "unavailable", "unrelated sites");
                    want(1, "co.uk", "unavailable", "unrelated sites");
                    // No absolute program path to bind to.
                    want(0, "weird", "unavailable", "program file");
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
                    console.warn("LEVEL_RULE_UNAVAILABLE_PROBE_DONE");
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
        "unavailable probe failed:\n{}",
        bad.join("\n")
    );
    assert!(
        captured.contains(DONE_MARKER),
        "probe never finished; stderr:\n{captured}"
    );
}
