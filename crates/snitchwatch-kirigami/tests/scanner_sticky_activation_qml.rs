//! Sticky failures and single activation, through real key/mouse events and a
//! counting fake `org.snitchwatch.Scanner1` broker on a private session bus.
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

static CALLS: AtomicUsize = AtomicUsize::new(0);

/// Call 1 fails at once; later calls take 600 ms and succeed.
struct FakeBroker;

#[zbus::interface(name = "org.snitchwatch.Scanner1")]
impl FakeBroker {
    async fn run_deep_scan(&self) -> (String, String) {
        if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            return ("failed".into(), "boom".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
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

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtTest
import com.snitchwatch.shell

Window {
    id: win
    visible: true
    width: 900; height: 700
    ScannerController { id: controller }
    ScannerPage { id: page; anchors.fill: parent; controller: controller }
    TestCase { id: tc; name: "scan"; when: false; windowShown: true }
    property int step: 0
    property int waited: 0
    property string failTitle: ""
    property string failSummary: ""
    function walk(root, out, seen) {
        if (!root || seen.has(root)) return out;
        seen.add(root);
        out.push(root);
        for (let i = 0; i < root.children.length; ++i) walk(root.children[i], out, seen);
        if (root.contentItem) walk(root.contentItem, out, seen);
        return out;
    }
    function named(n) { return walk(page, [], new Set()).find(i => i.objectName === n); }
    function visibleNamed(n) { const i = named(n); return !!i && i.visible && i.height > 0; }
    function report(state, detail) {
        console.warn("STICKY state=" + state + " " + detail);
        Qt.quit();
    }
    function check(cond, what) { if (!cond) throw new Error(what); }
    function focusRun() { const b = named("scannerRunButton"); b.forceActiveFocus(); return b; }
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            try {
                waited += 1;
                if (step === 0) {                       // idle: Space starts scan 1 (fails)
                    focusRun(); tc.keyClick(Qt.Key_Space); step = 1; waited = 0;
                } else if (step === 1 && !controller.busy && controller.errorTitle !== "") {
                    failTitle = controller.errorTitle; failSummary = controller.summaryText;
                    check(failSummary.indexOf("did not complete") >= 0, "no failure summary: " + failSummary);
                    check(visibleNamed("scannerStatusMessage"), "error header not visible");
                    focusRun(); tc.mouseClick(named("scannerRunButton"));   // scan 2 starts (slow)
                    step = 2; waited = 0;
                } else if (step === 2 && controller.busy) {
                    // The failure must stay up while the new scan is in flight.
                    check(controller.errorTitle === failTitle, "error wiped at scan start");
                    check(controller.summaryText === failSummary, "summary wiped at scan start");
                    check(visibleNamed("scannerStatusMessage"), "error header hidden while scanning");
                    check(visibleNamed("scannerBusy"), "no Scanning... indicator");
                    // Activations while busy must not start more scans.
                    focusRun(); tc.keyClick(Qt.Key_Return); tc.keyClick(Qt.Key_Space);
                    tc.mouseClick(named("scannerRunButton"));
                    step = 3; waited = 0;
                } else if (step === 3 && !controller.busy) {
                    check(controller.errorTitle === "", "error not cleared by success");
                    check(controller.summaryText.indexOf("scan #5") >= 0, "no success summary");
                    check(!visibleNamed("scannerStatusMessage"), "error header still visible");
                    focusRun(); tc.keyClick(Qt.Key_Space);  // after completion: scan 3
                    step = 4; waited = 0;
                } else if (step === 4 && controller.busy) {
                    step = 5; waited = 0;
                } else if (step === 5 && !controller.busy) {
                    report("done", "");
                } else if (waited > 80) {
                    report("stuck", "step=" + step + " busy=" + controller.busy + " err=" + controller.errorTitle);
                }
            } catch (e) { report("error", String(e)); }
        }
    }
    Timer { interval: 20000; running: true; onTriggered: report("timeout", "step=" + step) }
}
"#;

#[test]
fn failures_stay_until_the_next_outcome_and_activation_starts_one_scan() {
    init_headless_qt_env();
    let mut daemon = start_private_bus();
    serve_fake_broker();
    force_transport_for_tests(Transport::Broker);
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_, obj, _| {
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(
                &QByteArray::from(QML),
                &QUrl::from("qrc:/inline_sticky_probe.qml"),
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
        .find(|l| l.contains("STICKY"))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    assert!(line.contains("state=done"), "{line}");
    // Space on idle, a click after the failure, Space after completion; the
    // three activations made while busy started nothing.
    assert_eq!(CALLS.load(Ordering::SeqCst), 3, "{line}");
}
