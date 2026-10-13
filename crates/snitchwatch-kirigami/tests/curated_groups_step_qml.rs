//! The first-run "Recommended rules" step (`CuratedGroupsStep.qml`) with a real
//! `CuratedDefaultsModel`:
//!   * nothing is offered before the bridge sends groups, and a bridge without
//!     `curatedGroupsV1` keeps today's flat list (no group switches);
//!   * one switch per group, all off; a switch sends exactly that group's ids
//!     and keeps the bridge's value until it answers;
//!   * details show program, destination and port as plain text;
//!   * the name-resolution group goes through the DNS consent flow, never a
//!     plain group request;
//!   * skipping sends nothing.
//!
//! Fails on any QML warning from the probe or the shell's QML. Run headless
//! with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::curated_defaults_model as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/curated_groups_step_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

#[test]
fn the_step_offers_one_opt_in_switch_per_group_and_routes_dns_through_consent() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.ApplicationWindow {
    id: probeWindow
    visible: true
    width: 700
    height: 900

    property var sent: []
    property int finishedCount: 0
    readonly property var caps: { "action": "authenticated",
        "capabilities": ["curatedDefaults", "curatedGroupsV1", "curatedBroadOptInV1"] }

    function expect(condition, message) {
        if (!condition) {
            throw new Error(message);
        }
    }

    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = probeWindow.find(item.children[i], name);
            if (hit) return hit;
        }
        return null;
    }

    function entry(id, group, on, broad) {
        const e = { id: id, group: group, program: "/usr/bin/" + id,
                    allows: "<b>" + id + "</b> may connect to example.org on TCP port 443.",
                    why: "Why " + id, on: on, status: on ? "installed" : "off" };
        if (broad) e.broad = true;
        return e;
    }

    function send(on) {
        const groups = [
            { id: "system-services", title: "System services", description: "Clock and network." },
            { id: "app-updates", title: "App updates", description: "Flathub." },
            { id: "name-resolution", title: "Name resolution", description: "Any address." }
        ];
        curated.applyServerMessageJson(JSON.stringify({ action: "setCuratedDefaults",
            entries: [entry("chronyd", "system-services", on, false),
                      entry("flatpak", "app-updates", false, false),
                      entry("avahi", "system-services", on, false),
                      entry("dns-resolved", "name-resolution", false, true)],
            groups: groups, storage: { persistent: true } }));
    }

    function flip(name) {
        const sw = find(step, name);
        expect(sw !== null && sw.enabled, "no enabled switch " + name);
        sw.toggle();
        sw.toggled();
        return sw;
    }

    CuratedDefaultsModel { id: curated }

    Connections {
        target: curated
        function onCuratedChangeRequested(json) {
            probeWindow.sent.push(JSON.parse(json));
        }
    }

    CuratedGroupsStep {
        id: step
        anchors.fill: parent
        model: curated
        onFinished: probeWindow.finishedCount++
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                // Nothing offered before the bridge says so.
                expect(step.groupsRepeater.count === 0, "groups before any message");
                expect(find(step, "groupsFinish").text === "Skip", "no Skip");

                // Fallback: groups without the capability stay a flat list (no group UI).
                send(false);
                expect(!curated.grouped && step.groupsRepeater.count === 0,
                       "groups shown without curatedGroupsV1");
                expect(curated.count === 4, "flat rows: " + curated.count);

                // With the capability: one off switch per group, opt-in.
                curated.applyServerMessageJson(JSON.stringify(caps));
                send(false);
                expect(curated.grouped && step.groupsRepeater.count === 3,
                       "groups: " + step.groupsRepeater.count);
                for (const id of ["system-services", "app-updates", "name-resolution"]) {
                    expect(!find(step, "groupSwitch-" + id).checked, id + " is on by default");
                }
                const desc = find(step, "groupDescription-app-updates");
                expect(desc.text === "Flathub." && desc.textFormat === Text.PlainText,
                       "description is the bridge's, as plain text");
                expect(find(step, "groupsIntro").text.indexOf("off until you turn it on") >= 0,
                       "intro doesn't say rules are opt-in");

                // The switch sends exactly that group's ids; it keeps the bridge's value.
                const sw = flip("groupSwitch-system-services");
                expect(probeWindow.sent.length === 1, "sent: " + JSON.stringify(probeWindow.sent));
                const req = probeWindow.sent[0];
                expect(req.action === "setCuratedDefaults" && req.on === true
                       && JSON.stringify(req.ids.slice().sort()) === '["avahi","chronyd"]',
                       "group request: " + JSON.stringify(req));
                expect(!sw.checked, "switch claims on before the bridge says so");

                // Details: program, destination and port, as plain text.
                expect(find(step, "entryAllows") === null, "details shown unasked");
                find(step, "groupDetails-app-updates").clicked();
                const allows = find(step, "entryAllows");
                expect(allows !== null && allows.textFormat === Text.PlainText
                       && allows.text === "<b>flatpak</b> may connect to example.org on TCP port 443.",
                       "allows: " + (allows ? allows.text : null));
                expect(find(step, "entryProgram").text === "/usr/bin/flatpak", "program path");
                find(step, "groupDetails-app-updates").clicked();
                expect(find(step, "entryAllows") === null, "details stayed open");

                // DNS goes through the reviewed consent flow, never a plain group request.
                const before = probeWindow.sent.length;
                flip("groupSwitch-name-resolution");
                expect(curated.consentOpen, "no DNS consent dialog");
                expect(step.dnsConsentDialog.visible, "dialog not visible");
                const prepare = probeWindow.sent[probeWindow.sent.length - 1];
                expect(probeWindow.sent.length === before + 1
                       && prepare.action === "prepareCuratedBroadOptIn" && prepare.purpose === "enable",
                       "DNS did not ask for consent: " + JSON.stringify(probeWindow.sent));
                expect(probeWindow.sent.every(m => m.action !== "setCuratedDefaults"
                           || m.ids.indexOf("dns-resolved") < 0), "DNS sent as a group request");
                curated.cancelBroadConsent();

                // Skipping sends nothing more and finishes the step.
                const asked = probeWindow.sent.length;
                find(step, "groupsFinish").clicked();
                expect(probeWindow.finishedCount === 1, "Skip didn't finish");
                expect(probeWindow.sent.length === asked, "Skip sent something");

                // Once a group is on (the bridge says so) the button says Continue.
                send(true);
                expect(find(step, "groupSwitch-system-services").checked, "bridge's on not shown");
                expect(find(step, "groupsFinish").text === "Continue", "no Continue");
            } finally {
                Qt.quit();
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
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "curated groups probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "curated groups probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
