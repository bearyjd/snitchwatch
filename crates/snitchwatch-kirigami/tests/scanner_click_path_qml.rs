//! The real click path: `ScannerController.runScan()` -> worker thread ->
//! broker transport on a (private) session bus -> queued Qt update -> page.
//! A fake `org.snitchwatch.Scanner1` service stands in for the host broker.
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_kirigami::scanner::{force_transport_for_tests, Transport};

#[allow(unused_imports)]
use snitchwatch_kirigami::scanner_controller as _;
mod common;
use common::{capture_stderr, init_headless_qt_env};

const REPORT: &str = r#"{"scan_id":5,"new":[],"still_outstanding":[],"resolved":[],
"informational":[{"detail":"Secure Boot is currently enabled","path":"lockdown:secureboot"},
{"detail":"kernel lockdown is currently 'none'","path":"lockdown:kernel"}],
"skipped":[{"check":"file_drift","reason":"r"},{"check":"rootkit_signature","reason":"r"}]}"#;

struct FakeBroker;

#[zbus::interface(name = "org.snitchwatch.Scanner1")]
impl FakeBroker {
    async fn run_deep_scan(&self) -> (String, String) {
        ("ok".into(), REPORT.into())
    }
}

fn start_private_bus() -> std::process::Child {
    let mut daemon = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("dbus-daemon is needed for the click-path test");
    let mut address = String::new();
    BufReader::new(daemon.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address.trim());
    daemon
}

fn serve_fake_broker() {
    let (ready, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _conn = zbus::connection::Builder::session()
                    .unwrap()
                    .name(scanner_broker::BUS_NAME)
                    .unwrap()
                    .serve_at(scanner_broker::OBJECT_PATH, FakeBroker)
                    .unwrap()
                    .build()
                    .await
                    .unwrap();
                ready.send(()).unwrap();
                std::future::pending::<()>().await;
            });
    });
    wait.recv_timeout(std::time::Duration::from_secs(10))
        .expect("fake broker never came up");
}

#[test]
fn run_scan_over_the_broker_bus_draws_the_report() {
    init_headless_qt_env();
    let mut daemon = start_private_bus();
    serve_fake_broker();
    force_transport_for_tests(Transport::Broker);

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    visible: true
    width: 900; height: 700
    ScannerController { id: controller }
    ScannerPage { id: page; anchors.fill: parent; controller: controller }
    property bool started: false
    property bool sawBusy: false
    function walk(root, out, seen) {
        if (!root || seen.has(root)) return out;
        seen.add(root);
        out.push(root);
        for (let i = 0; i < root.children.length; ++i) walk(root.children[i], out, seen);
        if (root.contentItem) walk(root.contentItem, out, seen);
        return out;
    }
    function report(state, detail) {
        console.warn("CLICK_PATH state=" + state + " " + detail);
        Qt.quit();
    }
    function snapshot() {
        const all = walk(page, [], new Set());
        const found = all.filter(i => i.objectName === "scannerFindingPath" && i.visible).map(i => i.text);
        const heads = all.filter(i => typeof i.level === "number" && i.visible).map(i => i.text);
        return "busy=" + controller.busy + " sawBusy=" + sawBusy + " report=" + controller.reportJson.length
            + " errorTitle=" + JSON.stringify(controller.errorTitle) + " summary=" + JSON.stringify(controller.summaryText)
            + " findings=" + found.length + " headings=" + JSON.stringify(heads);
    }
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            try {
                if (!started) { started = true; walk(page, [], new Set()).find(i => i.objectName === "scannerRunButton").clicked(); }
                if (controller.busy) sawBusy = true;
                if (started && !controller.busy && (controller.reportJson.length > 0 || controller.errorTitle.length > 0)) {
                    report("done", snapshot());
                }
            } catch (e) { report("error", String(e)); }
        }
    }
    Timer { interval: 15000; running: true; onTriggered: report("timeout", snapshot()) }
}
"#;
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_, obj, _| {
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(
                &QByteArray::from(qml),
                &QUrl::from("qrc:/inline_click_path_probe.qml"),
            );
        }
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert!(
        root_ok.load(Ordering::SeqCst),
        "page failed to load: {captured}"
    );
    let line = captured
        .lines()
        .find(|l| l.contains("CLICK_PATH"))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    assert!(
        line.contains("state=done"),
        "click path did not finish: {line}"
    );
    assert!(line.contains("findings=4"), "report not drawn: {line}");
    assert!(
        line.contains("0 new, 0 still outstanding, 2 informational, 2 skipped"),
        "summary missing: {line}"
    );
}
