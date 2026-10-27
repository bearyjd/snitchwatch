//! The Daemon Health page's deny-mode section with a real `DenyStatusModel`
//! pointed at fixture files (never the real `/run` path, never the helper):
//! each state's text, plain-text blockers, and copy buttons only.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QDateTime, QGuiApplication, QQmlApplicationEngine, QString, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::deny_status_model as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

fn fixture(name: &str) -> String {
    format!(
        "{}/../../tests/fixtures/deny-status/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Unix seconds as the helper's `YYYY-MM-DDTHH:MM:SSZ` (Howard Hinnant's
/// `civil_from_days`).
fn utc_stamp(secs: i64) -> String {
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// A trial written now that ends five minutes from now, in the helper's format.
fn live_trial(dir: &std::path::Path) -> String {
    assert_eq!(utc_stamp(1_793_016_300), "2026-10-26T12:05:00Z");
    let now = QDateTime::current_secs_since_epoch();
    let text = std::fs::read_to_string(fixture("trial.json"))
        .unwrap()
        .replace("2026-10-26T12:05:00Z", &utc_stamp(now + 300))
        .replace("2026-10-26T12:00:00Z", &utc_stamp(now));
    let path = dir.join("trial-live.json");
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

#[test]
fn the_deny_section_shows_each_state_and_only_copy_buttons() {
    init_headless_qt_env();
    let dir = tempfile::tempdir().unwrap();
    let trial = live_trial(dir.path());
    // Rewritten from allow to confirmed once the window is hidden.
    let mutable = dir.path().join("mutable.json");
    std::fs::copy(fixture("allow.json"), &mutable).unwrap();
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Window
import com.snitchwatch.shell
import "qrc:/qt/qml/com/snitchwatch/shell/qml" as Shell
Window {
    id: root
    visible: true
    width: 640
    height: 900
    QtObject {
        id: health
        property bool hasProblem: false
        property string statusSummary: "ok"
        property string troubleshootingText: ""
        property string monitorStatus: "Process monitor: not reported by opensnitchd"
        function recheck() {}
    }
    DenyStatusModel {
        id: deny
        statusPath: "FIXTURE/absent.json"
        bootIdPath: "FIXTURE/boot_id"
    }
    Shell.DaemonHealthPage { id: page; model: health; denyStatus: deny }
    function findAll(item, test, found) {
        if (test(item)) found.push(item);
        for (const child of item.children) findAll(child, test, found);
        if (item.contentItem) findAll(item.contentItem, test, found);
        return found;
    }
    function find(name) {
        return findAll(page, i => i.objectName === name, [])[0];
    }
    function check(condition, message) { if (!condition) throw new Error(message); }
    function show(path) { deny.statusPath = path; deny.refresh(); }
    function fail(message) { console.warn("Error: " + message); Qt.quit(); }
    Timer { interval: 15000; running: true; onTriggered: root.fail("TIMEOUT") }
    // onCompleted hides the window; the test then rewrites the file from
    // allow to confirmed before the event loop starts, so the write is done
    // before this checks the hidden page still shows the old headline.
    Timer {
        id: hiddenWait
        interval: 2500
        onTriggered: {
            try {
                check(find("denyHeadline").text === "Deny mode: off (allow)",
                      "a change was read while hidden: " + find("denyHeadline").text);
                root.visible = true;
                check(find("denyStatusSection").refreshing, "refresh didn't resume on show");
                shownWait.start();
            } catch (e) { root.fail(e.message); }
        }
    }
    Timer {
        id: shownWait
        interval: 600
        onTriggered: {
            try {
                check(find("denyHeadline").text === "Deny mode: on (confirmed)",
                      "the change wasn't read after show: " + find("denyHeadline").text);
                console.info("DENY_STATUS_OK");
            } catch (e) { root.fail(e.message); return; }
            Qt.quit();
        }
    }
    Component.onCompleted: { try {
        show("FIXTURE/absent.json");
        check(deny.state === "unknown" && deny.asOf === "", "missing file not unknown");
        const headline = find("denyHeadline");
        check(headline && headline.text === "Deny mode: unknown", "headline missing: " + (headline && headline.text));
        show("FIXTURE/allow.json");
        check(deny.state === "allow" && deny.headline === "Deny mode: off (allow)", "allow: " + deny.headline);
        check(find("denyAsOf").text.indexOf("as of ") === 0, "no as-of time");
        show("TRIAL");
        check(deny.state === "trial", "live trial: " + deny.state + " " + deny.detail);
        // "(at HH:MM)", or with the date when the deadline falls tomorrow.
        check(/^Reverts to allow in [45] min \d+ s \(at (\d{4}-\d\d-\d\d )?\d\d:\d\d\) unless confirmed\.$/.test(find("denyDetail").text),
              "countdown: " + find("denyDetail").text);
        show("FIXTURE/trial.json");
        check(deny.state === "overdue" && find("denyHeadline").text === "Deny mode: revert overdue", "overdue: " + deny.state);
        show("FIXTURE/stale-boot.json");
        check(deny.state === "unknown" && deny.detail === "The status file is from an earlier boot.", "stale: " + deny.detail);
        show("FIXTURE/garbage.bin");
        check(deny.state === "unknown", "garbage: " + deny.state);
        show("FIXTURE/blocked.json");
        const blockers = find("denyBlockers");
        check(blockers.visible && blockers.textFormat === Text.PlainText
              && blockers.text.indexOf("<b>legacy TCP profile</b>") >= 0, "blockers: " + blockers.text);
        // Anything clickable (AbstractButton has checkable; MouseArea doesn't).
        const buttons = findAll(page, i => i.visible && i.pressed !== undefined
                                && i.checkable !== undefined, []);
        const copies = findAll(page, i => i.objectName === "copyDenyCommand", []);
        check(copies.length === 4, "copy buttons: " + copies.length + " of " + buttons.length
              + " buttons; " + buttons.map(b => b.text).join(","));
        // The page's own Recheck is the only other button: no action buttons.
        const others = buttons.filter(b => b.objectName !== "copyDenyCommand");
        check(others.length === 1 && others[0].text === "Recheck",
              "unexpected buttons: " + others.map(b => b.text).join(","));
        check(JSON.parse(deny.commandsJson).join("|") ===
              "ujust snitchwatch-deny status|ujust snitchwatch-deny on 10|ujust snitchwatch-deny confirm|ujust snitchwatch-deny off",
              "commands: " + deny.commandsJson);
        // The once-a-second re-read runs only while the window is shown.
        const section = find("denyStatusSection");
        check(section && section.refreshing, "not refreshing while shown");
        show("MUTABLE");
        check(find("denyHeadline").text === "Deny mode: off (allow)", "mutable: " + deny.headline);
        root.visible = false;
        check(!section.refreshing, "still refreshing while the window is hidden");
        hiddenWait.start();
    } catch (e) { root.fail(e.message); } }
}
"#
    .replace("FIXTURE/", &fixture(""))
    .replace("\"TRIAL\"", &format!("{trial:?}"))
    .replace("\"MUTABLE\"", &format!("{:?}", mutable.display().to_string()));

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });
    let stderr = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(
                &QByteArray::from(&qml),
                &QUrl::from(&QString::from("qrc:/deny_status_probe.qml")),
            );
        }
        if root_ok.load(Ordering::SeqCst) {
            // onCompleted ran inside load_data and hid the window; rewrite
            // now, finished before any timer can fire.
            let staged = mutable.with_extension("tmp");
            std::fs::copy(fixture("confirmed.json"), &staged).unwrap();
            std::fs::rename(&staged, &mutable).unwrap();
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);
    assert!(stderr.contains("DENY_STATUS_OK"), "{stderr}");
    assert!(root_ok.load(Ordering::SeqCst), "{stderr}");
    assert!(!stderr.contains("Error:"), "{stderr}");
}

/// The section stays small, under the shell's 400-line QML budget.
#[test]
fn the_deny_section_stays_within_the_qml_budget() {
    let lines = include_str!("../qml/DenyStatusSection.qml").lines().count();
    assert!(lines <= 400, "{lines}");
}
