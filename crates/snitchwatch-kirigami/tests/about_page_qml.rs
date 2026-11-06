//! The About page shows the values it is given, and its one button copies them.
//! Two pages share one window: one pointed at a Flatpak-info fixture with a
//! connected daemon, one with no such file and no daemon. A readiness gate
//! waits for the rows to be drawn, the copy button is clicked, and a second
//! gate waits for its confirmation; a watchdog timer ends the run regardless.
//! The clipboard is read back natively. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

use snitchwatch_kirigami::about::BuildInfo;
#[allow(unused_imports)]
use snitchwatch_kirigami::about_model as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
#[path = "../src/clipboard_native_test.rs"]
mod clipboard_native_ffi;
mod common;
use common::{capture_stderr, init_headless_qt_env};

const FLATPAK_INFO: &str = "[Application]\nname=org.snitchwatch.Snitchwatch\nruntime=runtime/org.kde.Platform/x86_64/6.8\n\n[Instance]\nbranch=stable\narch=x86_64\napp-commit=aaaa1111bbbb2222\n";
const MARKER: &str = "ABOUT_PAGE_RESULT";

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000; height: 800
    property var fp: null
    property var plain: null
    property int ticks: 0
    property bool clicked: false
    readonly property var interactive: [Accessible.Button, Accessible.CheckBox,
        Accessible.RadioButton, Accessible.ComboBox, Accessible.EditableText,
        Accessible.Slider, Accessible.SpinBox, Accessible.MenuItem,
        Accessible.ButtonMenu, Accessible.ButtonDropDown, Accessible.Link]

    function walk(root, out, seen) {
        if (!root || seen.has(root)) return out;
        seen.add(root);
        out.push(root);
        const kids = [];
        if (root.children) for (let i = 0; i < root.children.length; ++i) kids.push(root.children[i]);
        if (root.contentItem && root.contentItem !== root) kids.push(root.contentItem);
        for (const k of kids) walk(k, out, seen);
        return out;
    }
    function drawn(i) { return i.visible && i.width > 0 && i.height > 0; }
    function named(page, name) {
        return walk(page, [], new Set()).filter(i => i.objectName === name && drawn(i));
    }
    function values(page) {
        const out = {};
        for (const i of walk(page, [], new Set())) {
            if (i.objectName.startsWith("aboutValue_") && drawn(i))
                out[i.objectName.substring(11)] = i.text;
        }
        return out;
    }
    function make(props, x) {
        const c = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/AboutPage.qml");
        if (c.status !== Component.Ready) throw new Error(c.errorString());
        return c.createObject(contentItem, Object.assign({ x: x, width: 500, height: 800 }, props));
    }
    function report(state) {
        const buttons = walk(fp, [], new Set()).filter(i => probe.interactive.indexOf(i.Accessible.role) >= 0);
        const copy = named(fp, "aboutCopyButton")[0];
        console.warn("ABOUT_PAGE_RESULT " + JSON.stringify({
            state: state, ticks: ticks,
            fp: values(fp), plain: values(plain),
            copied: named(fp, "aboutCopiedNote").length,
            unnamed: buttons.filter(b => !b.Accessible.name).length,
            interactiveCount: buttons.length,
            tabbable: copy ? (copy.activeFocusOnTab && copy.focusPolicy !== Qt.NoFocus) : false,
            copyName: copy ? copy.Accessible.name : "",
            reportText: fp.about.reportText
        }));
        Qt.quit();
    }
    Component.onCompleted: {
        fp = make({ flatpakInfoPath: "@FIXTURE@", daemonConnected: true, daemonVersion: "1.8.0" }, 0);
        plain = make({ flatpakInfoPath: "@MISSING@", daemonConnected: false, daemonVersion: "" }, 500);
    }
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            ticks += 1;
            try {
                const ready = Object.keys(values(fp)).length >= 10 && Object.keys(values(plain)).length >= 5
                    && named(fp, "aboutCopyButton").length === 1;
                if (!ready) return;
                if (!clicked) {
                    clicked = true;
                    named(fp, "aboutCopyButton")[0].clicked();
                } else if (named(fp, "aboutCopiedNote").length === 1) {
                    report("ready");
                }
            } catch (e) {
                console.warn("ABOUT_PAGE_ERROR " + String(e));
                Qt.quit();
            }
        }
    }
    // Independent of the polling above, so nothing can leave the event loop running.
    Timer {
        interval: 8000; running: true
        onTriggered: report("timeout")
    }
}
"#;

#[test]
fn the_page_shows_the_given_values_and_copies_them() {
    init_headless_qt_env();
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("flatpak-info");
    std::fs::write(&fixture, FLATPAK_INFO).expect("fixture");
    let missing = dir.path().join("absent");
    let qml = QML
        .replace("@FIXTURE@", fixture.to_str().expect("utf8 path"))
        .replace("@MISSING@", missing.to_str().expect("utf8 path"));

    let mut app = QGuiApplication::new();
    // As an assistive technology would; Controls fill in their accessible role
    // and name only then.
    assert!(
        accessibility::ffi::activate_accessibility(),
        "Qt's accessibility layer did not turn on"
    );
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
                &QByteArray::from(qml.as_str()),
                &QUrl::from("qrc:/inline_about_probe.qml"),
            );
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
        "page failed to load: {captured}"
    );
    assert!(!captured.contains("Error:"), "QML error: {captured}");
    assert!(!captured.contains("ABOUT_PAGE_ERROR"), "{captured}");
    let line = captured
        .lines()
        .find_map(|l| {
            l.split_once(MARKER)
                .map(|(_, rest)| rest.trim().to_string())
        })
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    let v: serde_json::Value = serde_json::from_str(&line).expect("probe JSON");
    assert_eq!(v["state"], "ready", "not ready in time: {line}");

    let build = BuildInfo::current();
    let expected_commit = match (&build.commit, build.dirty) {
        (None, _) => "not recorded in this build".to_string(),
        (Some(c), false) => c.clone(),
        (Some(c), true) => format!("{c} (uncommitted changes)"),
    };
    let fp = &v["fp"];
    assert_eq!(fp["gui_version"], env!("CARGO_PKG_VERSION"), "{line}");
    assert_eq!(fp["build_commit"], expected_commit.as_str(), "{line}");
    assert_eq!(fp["flatpak"], "Running in Flatpak", "{line}");
    assert_eq!(fp["flatpak_app"], "org.snitchwatch.Snitchwatch", "{line}");
    assert_eq!(fp["flatpak_app_commit"], "aaaa1111bbbb2222", "{line}");
    assert_eq!(fp["flatpak_branch"], "stable", "{line}");
    assert_eq!(
        fp["flatpak_runtime"], "runtime/org.kde.Platform/x86_64/6.8",
        "{line}"
    );
    assert_eq!(fp["flatpak_runtime_commit"], "unknown", "{line}");
    assert_eq!(fp["daemon_version"], "1.8.0", "{line}");
    let qt = fp["qt_version"].as_str().expect("qt version");
    assert!(qt.starts_with("6."), "Qt version: {line}");

    let plain = &v["plain"];
    assert_eq!(plain["flatpak"], "Not running in Flatpak", "{line}");
    assert!(plain.get("flatpak_branch").is_none(), "{line}");
    assert_eq!(plain["daemon_version"], "not connected", "{line}");
    assert_eq!(plain["gui_version"], env!("CARGO_PKG_VERSION"), "{line}");

    // Every interactive control is named and reachable by keyboard.
    assert_eq!(v["interactiveCount"], 1, "{line}");
    assert_eq!(v["unnamed"], 0, "{line}");
    assert_eq!(v["copyName"], "Copy version info", "{line}");
    assert_eq!(v["tabbable"], true, "{line}");

    // The button put the whole block on the clipboard, as plain text.
    let copied = clipboard_native_ffi::ffi::clipboard_text().to_string();
    assert_eq!(copied, v["reportText"].as_str().expect("report"), "{line}");
    assert!(copied.starts_with("Snitchwatch version info\n"), "{copied}");
    for needle in [
        "Flatpak app commit: aaaa1111bbbb2222\n",
        "Flatpak branch: stable\n",
        "opensnitchd (daemon): 1.8.0\n",
        &format!("Snitchwatch GUI version: {}\n", env!("CARGO_PKG_VERSION")),
    ] {
        assert!(copied.contains(needle), "missing {needle:?} in {copied}");
    }
}
