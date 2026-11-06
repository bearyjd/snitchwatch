//! `GuardedMenuItem` (the Copy / Select all rows of `SelectableText` and
//! `RowCopyMenu`) is at least `Theme.hitMin` (44 px) in both directions even
//! when the desktop style would make it shorter. The fixture forces a compact
//! item (no padding, tiny font, a 10 px background) so only the clamp in
//! `GuardedMenuItem.qml` can keep it at 44 px. Run headless with
//! `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::about_model as _;
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const MARKER: &str = "GUARDED_MENU_ITEM_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 400; height: 300
    GuardedMenuItem {
        id: item
        text: "x"
        padding: 0; topPadding: 0; bottomPadding: 0; leftPadding: 0; rightPadding: 0
        topInset: 0; bottomInset: 0; leftInset: 0; rightInset: 0
        font.pixelSize: 8
        background: Item { implicitWidth: 10; implicitHeight: 10 }
    }
    Timer {
        interval: 200; running: true
        onTriggered: {
            console.warn("GUARDED_MENU_ITEM_RESULT " + JSON.stringify({
                hitMin: Theme.hitMin, ih: item.implicitHeight, iw: item.implicitWidth,
                h: item.height, w: item.width }));
            Qt.quit();
        }
    }
    Timer { interval: 20000; running: true; onTriggered: Qt.quit() }
}
"##;

#[test]
fn guarded_menu_item_is_at_least_hit_min_under_a_compact_style() {
    init_headless_qt_env();
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
                &QUrl::from("qrc:/inline_guarded_menu_item_probe.qml"),
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
        "probe failed to load: {captured}"
    );
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    let v: serde_json::Value = serde_json::from_str(&line).expect("probe JSON");
    let hit = v["hitMin"].as_f64().unwrap();
    assert!(hit >= 44.0, "Theme.hitMin: {line}");
    for k in ["ih", "iw", "h", "w"] {
        assert!(
            v[k].as_f64().unwrap() >= hit,
            "{k} under Theme.hitMin: {line}"
        );
    }
}
