//! "Simulate this connection", end to end: the Connections inspector's button
//! asks `ConnectionsModel.simulationPrefillJson` for the row's known fields
//! and the Simulate sheet takes them, leaving everything the row doesn't carry
//! **unknown** (a blank field), clearing whatever an earlier run left there.
//!
//! The Qt-free mapping is unit-tested in `rules::simulator::prefill`; this
//! probe covers what that can't: the button's wiring, the sheet's `prefill`,
//! and that an unknown still reaches the simulator as unknown (a rule that
//! needs it is reported as not evaluated, not as a miss). Fields are found by
//! `objectName` through the item tree. Assertions are QML `throw`s, which Qt
//! reports against the probe URL on stderr; the Rust side fails on any such
//! line (see `common::capture_stderr`).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;
use snitchwatch_kirigami::rules::simulator::DAEMON_PROTOCOLS;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/simulate_connection_probe.qml";

#[test]
fn the_inspector_prefills_the_simulator_and_unknowns_stay_unknown() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    // The sheet keeps its own copy of the daemon's protocol names; the probe
    // checks it against the Rust list (the placeholder below).
    let qml = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 1000
    height: 800

    property var requests: []

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

    function field(name) {
        const item = probeWindow.find(sheet.contentItem, name);
        probeWindow.expect(item !== null, "field not found: " + name);
        return item;
    }

    function connection(id, path, host, ip, port, protocol) {
        return { id: id, process: "curl", processPath: path, dstHost: host,
                 dstIp: ip, dstPort: port, protocol: protocol, direction: "outgoing",
                 action: "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 0 };
    }

    function rule(name, action, operand, data) {
        return { name: name, displayName: name, enabled: true, action: action,
                 duration: "always", description: "",
                 operator: { type: "simple", operand: operand, data: data,
                             sensitive: false, list: [] },
                 precedence: false, nolog: false };
    }

    // Open the inspector on a row and press its Simulate button, as a user
    // would; the page hands the prefill to the sheet like main.qml does.
    function simulateRow(row) {
        page.openInspector({
            rowId: row.id, process: row.process, host: row.dstHost, port: row.dstPort,
            protocol: row.protocol, verdict: "allowed", pending: false,
            matchedRule: "", matchedRuleDisplay: ""
        });
        const button = probeWindow.find(Controls.Overlay.overlay, "simulateConnectionButton");
        probeWindow.expect(button !== null, "no Simulate this connection button");
        button.clicked();
    }

    ConnectionsPage {
        id: page
        anchors.fill: parent
        model: ConnectionsModel { id: connections }
        onSimulateConnectionRequested: function (prefillJson) {
            probeWindow.requests.push(prefillJson);
            sheet.prefill(JSON.parse(prefillJson));
            sheet.open();
        }
    }

    Item {
        RuleSimulatorSheet {
            id: sheet
            model: RulesModel { id: rulesModel }
        }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                connections.applyServerMessageJson(JSON.stringify({
                    action: "insertConnectionRows",
                    rows: [
                        probeWindow.connection("r1", "/usr/bin/curl", "github.com", "140.82.112.3", 443, "tcp"),
                        // A bare-IP connection as the bridge's `connection_to_row` makes it:
                        // the daemon's DstHost is empty, so dstHost carries the IP.
                        probeWindow.connection("r2", null, "10.0.0.5", "10.0.0.5", 53, "udp"),
                        probeWindow.connection("r3", "/usr/bin/dig", "", "", 53, "udplite6"),
                        probeWindow.connection("r4", "/usr/bin/x", "x.example", "1.2.3.4", 80, "gre"),
                        // The bridge puts the IP in the host when the daemon had none.
                        probeWindow.connection("r5", "/usr/bin/dig", "10.0.0.9", "10.0.0.9", 53, "udp"),
                        probeWindow.connection("r6", "/usr/bin/nc", "10.0.0.9", "10.0.0.9", 443, "tcp")
                    ]
                }));
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "setRules",
                    rules: [
                        probeWindow.rule("050-uid", "deny", "user.id", "1000"),
                        probeWindow.rule("100-path", "deny", "process.path", "/usr/bin/curl"),
                        probeWindow.rule("200-host", "allow", "dest.host", "github.com")
                    ]
                }));
                probeWindow.expect(connections.simulationPrefillJson("nope") === "{}",
                    "an unknown row should prefill nothing");

                // The sheet offers exactly the protocol names the daemon gives
                // a connection (`DAEMON_PROTOCOLS`), after "unknown".
                probeWindow.expect(JSON.stringify(sheet.protocolNames) === '__DAEMON_PROTOCOLS__',
                    "protocolNames: " + JSON.stringify(sheet.protocolNames));
                const protocolBox = probeWindow.field("simProtocol");
                probeWindow.expect(protocolBox.count === sheet.protocolNames.length + 1,
                    "protocol entries: " + protocolBox.count);
                for (let i = 0; i < sheet.protocolNames.length; i++) {
                    probeWindow.expect(protocolBox.indexOfValue(sheet.protocolNames[i]) === i + 1,
                        "protocol not offered: " + sheet.protocolNames[i]);
                }

                // Something typed earlier must not survive into the next prefill.
                sheet.open();
                probeWindow.field("simUid").text = "1000";
                probeWindow.field("simEnv").text = "A=b";
                probeWindow.field("simChecksums").currentIndex = 1;
                probeWindow.field("simHostEmpty").checked = true;

                // A row that carries everything it can.
                probeWindow.simulateRow({ id: "r1", process: "curl", dstHost: "github.com",
                                          dstPort: 443, protocol: "tcp" });
                probeWindow.expect(probeWindow.requests.length === 1, "no prefill request");
                probeWindow.expect(probeWindow.field("simProcessPath").text === "/usr/bin/curl",
                    "path: " + probeWindow.field("simProcessPath").text);
                probeWindow.expect(probeWindow.field("simHost").text === "github.com", "host");
                probeWindow.expect(probeWindow.field("simHostEmpty").checked === false,
                    "no host name stayed ticked through the prefill");
                probeWindow.expect(probeWindow.field("simDestIp").text === "140.82.112.3", "ip");
                probeWindow.expect(probeWindow.field("simPort").value === 443, "port");
                probeWindow.expect(probeWindow.field("simProtocol").currentValue === "tcp", "protocol");
                probeWindow.expect(probeWindow.field("simUid").text === "", "uid survived the prefill");
                probeWindow.expect(probeWindow.field("simEnv").text === "", "env survived the prefill");
                probeWindow.expect(probeWindow.field("simChecksums").currentIndex === 0,
                    "checksum mode survived the prefill");
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === "100-path",
                    "r1 decided by: " + sheet.simulateMatchedRule);
                // The uid isn't carried, so the deny that needs it is not evaluated.
                probeWindow.expect(sheet.simulateUnevaluated.indexOf("050-uid") >= 0,
                    "uid should be unknown: " + sheet.simulateUnevaluated);

                // A row with no process path: unknown, not an empty path.
                probeWindow.simulateRow({ id: "r2", process: "curl", dstHost: "10.0.0.5",
                                          dstPort: 53, protocol: "udp" });
                probeWindow.expect(probeWindow.field("simProcessPath").text === "",
                    "the process name leaked into the path");
                probeWindow.expect(probeWindow.field("simDestIp").text === "10.0.0.5", "r2 ip");
                // The IP is the row's fallback for a missing host, not the
                // host: the daemon's DstHost for a bare IP is "".
                probeWindow.expect(probeWindow.field("simHost").text === "",
                    "r2 host: " + probeWindow.field("simHost").text);
                probeWindow.expect(probeWindow.field("simProtocol").currentValue === "udp", "r2 protocol");
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === "",
                    "r2 matched " + sheet.simulateMatchedRule);
                probeWindow.expect(sheet.simulateUnevaluated.indexOf("100-path") >= 0,
                    "an unknown path should leave the rule not evaluated, not a miss: "
                    + sheet.simulateUnevaluated);

                // A DNS query whose name is the IP: the host is unknown, so a
                // rule on the host is not evaluated. The same IP in the host
                // on another port is the bridge's stand-in for "no host name".
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "setRules",
                    rules: [probeWindow.rule("100-host", "deny", "dest.host", "10.0.0.9")]
                }));
                probeWindow.simulateRow({ id: "r5", process: "dig", dstHost: "10.0.0.9",
                                          dstPort: 53, protocol: "udp" });
                probeWindow.expect(probeWindow.field("simHost").text === ""
                                   && probeWindow.field("simHostEmpty").checked === false,
                    "r5: host should be unknown");
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === ""
                                   && sheet.simulateUnevaluated.indexOf("100-host") >= 0,
                    "r5: an unknown host should leave the rule not evaluated: " + sheet.simulateUnevaluated);
                probeWindow.simulateRow({ id: "r6", process: "nc", dstHost: "10.0.0.9",
                                          dstPort: 443, protocol: "tcp" });
                probeWindow.expect(probeWindow.field("simHost").text === ""
                                   && probeWindow.field("simHostEmpty").checked === true,
                    "r6: host should be known empty");
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === "" && sheet.simulateUnevaluated === "",
                    "r6: an empty host is known, so the rule just doesn't match: " + sheet.simulateUnevaluated);
                // Ticking the box disables the text, and a prefill clears it.
                sheet.prefill({});
                probeWindow.expect(probeWindow.field("simHostEmpty").checked === false,
                    "an empty prefill left the box ticked");

                // No destination IP known, and an IPv6-flavoured protocol.
                probeWindow.simulateRow({ id: "r3", process: "dig", dstHost: "",
                                          dstPort: 53, protocol: "udplite6" });
                probeWindow.expect(probeWindow.field("simDestIp").text === "", "r3 ip should be blank");
                probeWindow.expect(probeWindow.field("simProtocol").currentValue === "udplite6",
                    "r3 protocol: " + probeWindow.field("simProtocol").currentValue);

                // A protocol the daemon doesn't name is unknown, not a guess.
                probeWindow.simulateRow({ id: "r4", process: "x", dstHost: "x.example",
                                          dstPort: 80, protocol: "gre" });
                probeWindow.expect(probeWindow.field("simProtocol").currentValue === "",
                    "r4 protocol: " + probeWindow.field("simProtocol").currentValue);
                // A prefill that names a protocol the sheet doesn't offer, or
                // none at all, selects "unknown" rather than keeping the old one.
                for (const odd of [{ protocol: "weird", destPort: 80 }, { destPort: 80 }]) {
                    probeWindow.field("simProtocol").currentIndex = 3;
                    sheet.prefill(odd);
                    probeWindow.expect(probeWindow.field("simProtocol").currentValue === "",
                        "odd prefill protocol: " + probeWindow.field("simProtocol").currentValue);
                }
                // Nothing carried at all is not "from a connection".
                sheet.prefill({});
                probeWindow.expect(!sheet.prefilled, "an empty prefill claimed a connection");
                rulesModel.applyServerMessageJson(JSON.stringify({
                    action: "setRules",
                    rules: [probeWindow.rule("100-proto", "deny", "protocol", "tcp")]
                }));
                sheet.runSimulation();
                probeWindow.expect(sheet.simulateMatchedRule === ""
                                   && sheet.simulateUnevaluated.indexOf("100-proto") >= 0,
                    "unknown protocol should not be compared: " + sheet.simulateUnevaluated);
            } finally {
                Qt.quit();
            }
        }
    }
}
"#
    .replace(
        "__DAEMON_PROTOCOLS__",
        &serde_json::to_string(&DAEMON_PROTOCOLS).expect("protocol names serialize"),
    );

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
        // Without a root object nothing calls Qt.quit(): skip the loop and
        // let the assertion below report the load failure instead of hanging.
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "Simulate-this-connection probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "Simulate-this-connection probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}

/// The button's text is fixed, and the page stays within the size budget.
#[test]
fn the_inspector_button_is_fixed_text_and_the_page_stays_small() {
    let source = include_str!("../qml/ConnectionsPage.qml");
    assert!(
        source.contains("text: \"Simulate this connection\""),
        "ConnectionsPage.qml lost the Simulate this connection button"
    );
    let lines = source.lines().count();
    assert!(lines < 800, "ConnectionsPage.qml is {lines} lines");
}
