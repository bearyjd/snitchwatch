//! Restyle batch E: the real `ConnectionsPage` list under a deliberately
//! compact font (so only the clamp in `ConnectionRow.qml` can reach 44 px).
//! Every row (leaf, pending leaf, group header) is >= `Theme.hitMin` high; every
//! button of ours inside a row is >= 44 px both ways; the one selection
//! highlight is rounded from `Theme`, inset, ringed only while the list has
//! keyboard focus, and its ring is opaque, wider than 0, not equal to its
//! fill and contrasts with the list background by >= 2.0; the same holds
//! mirrored (RTL). `DecideLaterButton` and `RuleButton` (its inner `ruleButton`) belong to no restyle
//! batch yet and are reported, not asserted.
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const MARKER: &str = "ROUNDED_E_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Controls.ApplicationWindow {
    id: probe
    visible: true
    width: 1300; height: 800
    font.pixelSize: 6
    property int phase: 0
    property var out: ({ rows: [] })
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
    function row(id, host, pending) {
        return { id: id, process: "app", processPath: "/usr/bin/app", dstHost: host,
                 dstIp: "1.1.1.1", dstPort: 443, protocol: "tcp", direction: "outgoing",
                 action: pending ? null : "allow", bytesSent: 0, bytesReceived: 0, startedAtMs: 1 };
    }
    function send(action, key, values) {
        const m = { action: action };
        if (key) m[key] = values;
        model.applyServerMessageJson(JSON.stringify(m));
    }
    function walk(root, acc) {
        if (!root) return acc;
        acc.push(root);
        for (const k of (root.children || [])) walk(k, acc);
        return acc;
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
    function snapshot(tag) {
        const list = page.connectionList;
        const rows = [];
        for (let i = 0; i < list.count; ++i) {
            const it = list.itemAtIndex(i);
            if (!it) continue;
            const buttons = walk(it, []).filter(x => x.down !== undefined
                && x.visible && x.width > 0 && x.objectName !== "decideLaterButton"
                && x.objectName !== "headerRuleButton" && x.objectName !== "ruleButton" && x.hovered !== undefined && x.clicked !== undefined)
                .map(b => ({ name: b.objectName || b.text, w: b.width, h: b.height }));
            rows.push({ tag: tag, header: it.isGroupHeader, pending: it.pending || it.groupPending > 0,
                        h: it.height, ih: it.implicitHeight, buttons: buttons });
        }
        out.rows = out.rows.concat(rows);
    }
    function highlightFacts(tag) {
        const list = page.connectionList;
        list.currentIndex = 0;
        list.forceActiveFocus(Qt.TabFocusReason);
        const hi = list.highlightItem;
        const rect = hi ? walk(hi, []).find(x => x.objectName === "selectionHighlight") : null;
        if (!rect) { out["hl" + tag] = null; return; }
        const bg = rect.Kirigami.Theme.backgroundColor;
        const fill = over(rect.color, bg);
        const ringOver = over(rect.border.color, fill);
        out["hl" + tag] = { radius: rect.radius, x: rect.x, w: rect.width, hostW: hi.width,
            mirrored: rect.LayoutMirroring.enabled, ringWidth: rect.border.width,
            ringAlpha: rect.border.color.a * rect.opacity,
            ringEqFill: Qt.colorEqual(rect.border.color, rect.color),
            contrast: ratio(ringOver, fill), contrastBg: ratio(ringOver, bg),
            focused: list.activeFocus };
        list.focus = false;
        page.forceActiveFocus(Qt.MouseFocusReason);
        out["hlOff" + tag] = rect.border.width;
        list.forceActiveFocus(Qt.TabFocusReason);
    }
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            try {
                const n = phase++;
                if (n === 0) {
                    MapBandState.collapsed = true;
                    model.setGroupedMode(false);
                    send("insertConnectionRows", "rows", [row("a", "a.example.com", false), row("b", "b.example.com", true)]);
                } else if (n === 3) {
                    snapshot("flat");
                    highlightFacts("");
                    page.LayoutMirroring.enabled = true;
                    page.LayoutMirroring.childrenInherit = true;
                } else if (n === 5) {
                    highlightFacts("Rtl");
                    page.LayoutMirroring.enabled = false;
                    model.setGroupedMode(true);
                } else if (n === 8) {
                    snapshot("grouped");
                    out.hitMin = Theme.hitMin;
                    out.state = "ready";
                    console.warn("ROUNDED_E_RESULT " + JSON.stringify(out));
                    Qt.quit();
                }
            } catch (e) {
                console.warn("ROUNDED_E_ERROR " + String(e));
                Qt.quit();
            }
        }
    }
    Timer { interval: 40000; running: true; onTriggered: { console.warn("ROUNDED_E_ERROR timeout"); Qt.quit(); } }
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
                &QUrl::from("qrc:/inline_rounded_batch_e_probe.qml"),
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
    assert!(!captured.contains("ROUNDED_E_ERROR"), "{captured}");
    assert!(!captured.contains("TypeError"), "{captured}");
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    serde_json::from_str(&line).expect("probe JSON")
}

#[test]
fn rows_buttons_and_highlight_meet_the_rounded_spec() {
    let v = run_probe();
    let line = v.to_string();
    let hit = v["hitMin"].as_f64().unwrap();
    assert!(hit >= 44.0, "Theme.hitMin: {line}");
    let rows = v["rows"].as_array().unwrap();
    assert!(
        rows.iter().any(|r| r["header"] == false),
        "no leaf rows measured: {line}"
    );
    assert!(
        rows.iter().any(|r| r["header"] == true),
        "no header rows measured: {line}"
    );
    assert!(
        rows.iter().any(|r| r["pending"] == true),
        "no pending row measured: {line}"
    );
    for r in rows {
        assert!(
            r["h"].as_f64().unwrap() >= hit,
            "row under Theme.hitMin: {r}"
        );
        for b in r["buttons"].as_array().unwrap() {
            assert!(
                b["w"].as_f64().unwrap() >= hit && b["h"].as_f64().unwrap() >= hit,
                "button hit box under 44 px: {b} in {r}"
            );
        }
    }
    let buttons: usize = rows
        .iter()
        .map(|r| r["buttons"].as_array().unwrap().len())
        .sum();
    assert!(buttons >= 5, "buttons found: {buttons}: {line}");

    for tag in ["hl", "hlRtl"] {
        let h = &v[tag];
        assert!(h.is_object(), "no selection highlight ({tag}): {line}");
        assert_eq!(
            h["radius"].as_f64().unwrap(),
            10.0,
            "radius is not radiusMedium: {h}"
        );
        assert!(
            h["x"].as_f64().unwrap() > 0.0
                && h["w"].as_f64().unwrap() < h["hostW"].as_f64().unwrap(),
            "not inset: {h}"
        );
        assert_eq!(h["focused"], true, "list lost focus: {h}");
        assert_eq!(
            h["mirrored"],
            tag == "hlRtl",
            "mirroring did not reach the highlight: {h}"
        );
        assert!(h["ringWidth"].as_f64().unwrap() > 0.0, "ring width: {h}");
        assert!(
            h["ringAlpha"].as_f64().unwrap() > 0.5,
            "ring too transparent: {h}"
        );
        assert_eq!(h["ringEqFill"], false, "ring equals fill: {h}");
        assert!(
            h["contrast"].as_f64().unwrap() >= 2.0,
            "ring vs fill contrast: {h}"
        );
    }
    assert_eq!(
        v["hlOff"].as_f64().unwrap(),
        0.0,
        "ring shown without list focus"
    );
}
