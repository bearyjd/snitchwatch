//! Restyle leftovers: `InspectorPanel.qml` (close tool button, "Show rule",
//! "Simulate this connection"), `DecideLaterButton.qml` and `RuleButton.qml`
//! (its inner `ruleButton`). Under a deliberately compact font (6 px, so only
//! `Theme.hitMin` can reach 44 px) every one is >= 44 px both ways, is drawn by
//! a `RoundedSurface` whose keyboard focus ring is wider than 0, opaque,
//! different from the surface's fill and contrasts with it by >= 2.0; the
//! same holds mirrored (RTL). A source guard pins which plain controls remain
//! in the three files (none) and that they hold no literal colour.
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const MARKER: &str = "ROUNDED_INSP_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtTest
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Controls.ApplicationWindow {
    id: probe
    visible: true
    width: 1300; height: 800
    font.pixelSize: 6
    property int phase: 0
    property var out: ({ items: [] })
    QtObject {
        id: feedStub
        property bool ok: true
        function appBoundRulesFor() { return true; }
        function submitVerdict() { return true; }
    }
    ConnectionsModel { id: model }
    ConnectionsPage {
        id: page
        anchors.fill: parent
        model: model
        bridgeFeed: feedStub
    }
    TestCase { id: tc; name: "insp"; when: false }
    Item {
        id: stand
        x: 0; y: 0; width: 600; height: 300
        z: 100
        Row {
            spacing: 10
            DecideLaterButton { id: standLater; objectName: "standLater" }
            RuleButton { id: standNone; objectName: "standNone"; ruleState: "none" }
            RuleButton { id: standAllow; objectName: "standAllow"; ruleState: "allow" }
        }
    }
    function row(id, host, pending, rule) {
        return { id: id, process: "app", processPath: "/usr/bin/app", dstHost: host,
                 dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: pending ? null : "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1,
                 matchedRule: rule };
    }
    function walk(root, acc) {
        if (!root) return acc;
        acc.push(root);
        for (const k of (root.children || [])) walk(k, acc);
        return acc;
    }
    function find(name) { return walk(probe.contentItem, []).find(x => x.objectName === name) || null; }
    function findText(text) {
        return walk(page, []).find(x => x.text === text && typeof x.clicked === "function" && x.visible) || null;
    }
    function lum(c) {
        const f = v => v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
        return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b);
    }
    function ratio(a, b) {
        const x = lum(a), y = lum(b);
        return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
    }
    function over(top, bottom) {
        const a = top.a;
        return Qt.rgba(top.r * a + bottom.r * (1 - a), top.g * a + bottom.g * (1 - a),
                       top.b * a + bottom.b * (1 - a), 1);
    }
    function measure(tag, b, rtl) {
        if (!b) { out.items.push({ tag: tag, missing: true }); return; }
        b.forceActiveFocus(Qt.TabFocusReason);
        const s = b.background;
        const ring = s && s.focusRing ? s.focusRing : null;
        const surface = s && s.body ? Qt.rgba(b.palette.window.r, b.palette.window.g, b.palette.window.b, 1) : null;
        let item = { tag: tag, rtl: rtl, w: b.width, h: b.height,
                     type: String(b).split("(")[0], hasSurface: !!ring };
        if (ring) {
            const fill = over(s.body.visible ? s.body.color : Qt.rgba(0, 0, 0, 0), surface);
            item.ringOn = ring.visible && b.visualFocus;
            item.ringWidth = ring.border.width;
            item.ringAlpha = ring.border.color.a * s.opacity;
            item.ringEqFill = s.body.visible && Qt.colorEqual(ring.border.color, s.body.color);
            item.contrast = ratio(over(ring.border.color, fill), fill);
        }
        out.items.push(item);
    }
    function measureAll(rtl) {
        const sfx = rtl ? "Rtl" : "";
        measure("standLater" + sfx, standLater, rtl);
        measure("standNone" + sfx, standNone.button, rtl);
        measure("standAllow" + sfx, standAllow.button, rtl);
        measure("close" + sfx, find("inspectorCloseButton"), rtl);
        measure("simulate" + sfx, find("simulateConnectionButton"), rtl);
        measure("showRule" + sfx, findText("Show rule"), rtl);
        if (rtl) {
            const g = walk(standNone, []).find(x => x.objectName === "ruleGlyph");
            const l = walk(standNone, []).find(x => x.objectName === "ruleLabel");
            out.glyphRightOfLabel = g.mapToItem(standNone, 0, 0).x > l.mapToItem(standNone, 0, 0).x;
            out.rootH = standNone.height; out.rootImplicitH = standNone.implicitHeight;
        }
    }
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            try {
                const n = phase++;
                if (n === 0) {
                    MapBandState.collapsed = true;
                    model.setGroupedMode(false);
                    model.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows",
                        rows: [row("a", "a.example.com", false, "my-rule")] }));
                } else if (n === 3) {
                    const d = page.connectionList.itemAtIndex(0);
                    tc.mouseClick(d, 2, 2);
                } else if (n === 5) {
                    measureAll(false);
                    stand.LayoutMirroring.enabled = true;
                    stand.LayoutMirroring.childrenInherit = true;
                    page.LayoutMirroring.enabled = true;
                    page.LayoutMirroring.childrenInherit = true;
                } else if (n === 7) {
                    measureAll(true);
                    out.hitMin = Theme.hitMin;
                    console.warn("ROUNDED_INSP_RESULT " + JSON.stringify(out));
                    Qt.quit();
                }
            } catch (e) {
                console.warn("ROUNDED_INSP_ERROR " + String(e));
                Qt.quit();
            }
        }
    }
    Timer { interval: 40000; running: true; onTriggered: { console.warn("ROUNDED_INSP_ERROR timeout"); Qt.quit(); } }
}
"##;

fn run_probe() -> serde_json::Value {
    init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");
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
                &QUrl::from("qrc:/inline_rounded_inspector_probe.qml"),
            );
        }
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);
    assert!(root_ok.load(Ordering::SeqCst), "probe failed to load: {captured}");
    assert!(!captured.contains("ROUNDED_INSP_ERROR"), "{captured}");
    assert!(!captured.contains("TypeError"), "{captured}");
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    serde_json::from_str(&line).expect("probe JSON")
}

#[test]
fn inspector_decide_later_and_rule_buttons_meet_the_rounded_spec() {
    let v = run_probe();
    let line = v.to_string();
    let hit = v["hitMin"].as_f64().unwrap();
    assert!(hit >= 44.0, "Theme.hitMin: {line}");
    let items = v["items"].as_array().unwrap();
    for tag in ["standLater", "standNone", "standAllow", "close", "simulate", "showRule"] {
        for sfx in ["", "Rtl"] {
            let name = format!("{tag}{sfx}");
            let it = items
                .iter()
                .find(|i| i["tag"] == name.as_str())
                .unwrap_or_else(|| panic!("{name} not measured: {line}"));
            assert!(it["missing"].is_null(), "{name} not found: {line}");
            assert!(it["w"].as_f64().unwrap() >= hit, "{name} narrower than hitMin: {it}");
            assert!(it["h"].as_f64().unwrap() >= hit, "{name} shorter than hitMin: {it}");
            assert_eq!(it["hasSurface"], true, "{name} is not drawn by a RoundedSurface: {it}");
            assert_eq!(it["ringOn"], true, "{name}: no ring on keyboard focus: {it}");
            assert!(it["ringWidth"].as_f64().unwrap() > 0.0, "{name} ring width: {it}");
            assert!(it["ringAlpha"].as_f64().unwrap() > 0.5, "{name} ring too transparent: {it}");
            assert_eq!(it["ringEqFill"], false, "{name} ring equals fill: {it}");
            assert!(it["contrast"].as_f64().unwrap() >= 2.0, "{name} ring contrast: {it}");
        }
    }
    assert_eq!(v["glyphRightOfLabel"], true, "RuleButton content did not mirror: {line}");
    assert!(
        v["rootH"].as_f64().unwrap() >= hit && v["rootImplicitH"].as_f64().unwrap() >= hit,
        "RuleButton root under hitMin: {line}"
    );
}

fn qml_source(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("qml").join(format!("{name}.qml"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn count(src: &str, opener: &str) -> usize {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//") && l.contains(opener))
        .count()
}

/// (file, RoundedButton, GuardedToolButton). No plain Button / ToolButton /
/// Switch / RadioButton / CheckBox / ComboBox remains in any of them: the
/// panel's other controls are Labels, `SelectableText` and Kirigami items
/// (FormLayout, InlineMessage, PlaceholderMessage), which are not restylable.
const FILES: [(&str, usize, usize); 3] = [
    ("InspectorPanel", 2, 1),
    ("DecideLaterButton", 1, 0),
    ("RuleButton", 1, 0),
];

#[test]
fn the_three_files_hold_no_plain_buttons_and_no_literal_colours() {
    for (file, rounded, guarded_tool) in FILES {
        let src = qml_source(file);
        for plain in [
            "Controls.Button {",
            "Controls.ToolButton {",
            "Controls.Switch {",
            "Controls.RadioButton {",
            "Controls.CheckBox {",
            "Controls.ComboBox {",
        ] {
            assert_eq!(count(&src, plain), 0, "{file}: plain `{plain}`");
        }
        assert_eq!(count(&src, "RoundedButton {"), rounded, "{file}: RoundedButton");
        assert_eq!(count(&src, "GuardedToolButton {"), guarded_tool, "{file}: GuardedToolButton");
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!code.contains("Qt.rgba(") && !code.contains("\"#"), "{file}: literal colour");
    }
}
