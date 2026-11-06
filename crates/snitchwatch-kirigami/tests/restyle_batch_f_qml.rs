//! Rounded-style batch F (badges, chips, tooltip, the background bar, the map
//! marker, the learning banner; plan `2026-10-30-rounded-ios-style.md`).
//!
//! * `MapMarker`: its VISIBLE marker stays small, but the hit region (the
//!   item's `contains()`, which the scene graph uses to route clicks and
//!   hover) is at least `Theme.hitMin` in both directions.
//! * `GroupPendingBadge` / `GroupVerdictCounts` chips are capsules drawn by
//!   `RoundedSurface` and stay SMALL: they sit on a clickable group-header row
//!   (the row is the hit target) and take no focus, so they are not inflated.
//! * `BackgroundGroupBar`'s four buttons are restyled in place (type name
//!   still `Button`) and at least `Theme.hitMin` high.
//! * Where a focus ring is drawn it never equals the fill it sits on.
//! * Source guards: radii from `Theme`, palette-only colours, no input area.
//!
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::about_model as _;
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const MARKER: &str = "RESTYLE_F_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 900; height: 700
    property var stage: null
    property var barModel: null

    function cluster(dominant, total) {
        return { members: [{ kind: "country", name: "Testland" }], total: total,
                 dominant: dominant, mixed: false, summary: "Testland: " + total };
    }
    function make(url, props) {
        const c = Qt.createComponent(url);
        if (c.status !== Component.Ready) throw new Error(c.errorString());
        return c.createObject(stage, props);
    }
    function walk(root, out) {
        if (!root || out.indexOf(root) >= 0) return out;
        out.push(root);
        for (const k of (root.children || [])) walk(k, out);
        if (root.background) walk(root.background, out);
        return out;
    }
    function typeOf(o) { return String(o).split("(")[0]; }

    function markerReport() {
        const out = [];
        let x = 20;
        for (const d of ["allowed", "denied", "pending", "none"]) {
            for (const total of [1, 5000]) {
                const m = make("qrc:/qt/qml/com/snitchwatch/shell/qml/MapMarker.qml",
                    { cluster: cluster(d, total), x: x, y: 300 });
                x += 80;
                const cx = m.width / 2, cy = m.height / 2, e = Theme.hitMin / 2;
                const half = Math.max(m.width, Theme.hitMin) / 2;
                const pt = (dx, dy) => m.contains(Qt.point(cx + dx, cy + dy));
                const at = (dx, dy) => stage.childAt(m.x + cx + dx, m.y + cy + dy) === m;
                const halos = walk(m.background, []).filter(i => i.objectName === "focusHalo");
                out.push({
                    dominant: d, total: total, w: m.width, h: m.height, diameter: m.diameter,
                    small: smallGap,
                    cornersIn: [pt(-e + 1, -e + 1), pt(e - 1, -e + 1), pt(-e + 1, e - 1), pt(e - 1, e - 1)],
                    sidesIn: [pt(-e + 1, 0), pt(e - 1, 0), pt(0, -e + 1), pt(0, e - 1)],
                    outside: [pt(half + 2, 0), pt(-half - 2, 0), pt(0, half + 2), pt(0, -half - 2)],
                    childAtIn: [at(-e + 1, -e + 1), at(e - 1, e - 1), at(e - 1, -e + 1)],
                    childAtOut: [at(half + 2, 0), at(0, -half - 2)],
                    haloCount: halos.length,
                    haloIsVerdict: halos.length > 0 && Qt.colorEqual(halos[0].border.color, m.verdictColor),
                    focusable: m.activeFocusOnTab, role: m.Accessible.role
                });
            }
        }
        return out;
    }
    readonly property int smallGap: Theme.spacingSmall

    function badgeReport() {
        const b = make("qrc:/qt/qml/com/snitchwatch/shell/qml/GroupPendingBadge.qml", { count: 3, x: 20, y: 20 });
        const s = walk(b, []).filter(i => i.cornerRadius !== undefined);
        return { w: b.width, h: b.height, iw: b.implicitWidth, ih: b.implicitHeight,
                 surfaces: s.length, pill: s.length > 0 && s[0].pill && s[0].cornerRadius === Theme.radiusPill,
                 radius: s.length > 0 ? s[0].body.radius : -1, tab: b.activeFocusOnTab, focusPolicy: b.focusPolicy };
    }
    function countsReport() {
        const row = Qt.createQmlObject('import QtQuick; Item { property bool isGroupHeader: true; property int depth: 0;'
            + ' property string groupLastHour: \'[{"kind":"allowed","text":"2 allowed"},{"kind":"denied","text":"1 denied"}]\';'
            + ' property string groupToday: \'[]\'; width: 800; height: 80 }', stage);
        row.x = 20; row.y = 80;
        const c = make("qrc:/qt/qml/com/snitchwatch/shell/qml/GroupVerdictCounts.qml", { src: row, parent: row });
        const s = walk(c, []).filter(i => i.cornerRadius !== undefined && i.focusRing !== undefined);
        return { surfaces: s.length,
                 allPill: s.every(x => x.pill && x.cornerRadius === Theme.radiusPill),
                 maxH: Math.max(0, ...s.map(x => x.height)), tabs: walk(c, []).filter(i => i.activeFocusOnTab === true).length };
    }
    function barReport() {
        barModel = Qt.createQmlObject('import QtQuick; QtObject { property bool filterNeedsAttention: true;'
            + ' property int backgroundCount: 3; property int backgroundPrograms: 2; property bool backgroundAvailable: true;'
            + ' property int newProgramCount: 2; property string baselineNotice: "n"; property int matchingCount: 1;'
            + ' property int totalCount: 4; function setNeedsAttention(v) {} function markNewProgramsSeen() {}'
            + ' function dismissBaselineNotice() {} }', probe);
        const bar = make("qrc:/qt/qml/com/snitchwatch/shell/qml/BackgroundGroupBar.qml",
            { model: barModel, width: 700, x: 20, y: 200 });
        const out = [];
        for (const n of ["showAllButton", "backgroundToggle", "markSeenButton", "dismissBaseline"]) {
            const b = walk(bar, []).filter(i => i.objectName === n)[0];
            const s = b ? walk(b, []).filter(i => i.cornerRadius !== undefined && i.focusRing !== undefined) : [];
            out.push({ name: n, found: !!b, h: b ? b.height : -1, w: b ? b.width : -1,
                       iw: b ? b.implicitWidth : -1, type: b ? typeOf(b) : "",
                       surfaces: s.length, pill: s.length > 0 && s[0].cornerRadius === Theme.radiusPill });
        }
        return out;
    }

    Item { id: stageItem; anchors.fill: parent }
    Component.onCompleted: stage = stageItem
    Timer {
        interval: 200; running: true
        onTriggered: {
            try {
                console.warn("RESTYLE_F_RESULT " + JSON.stringify({
                    hitMin: Theme.hitMin, markers: markerReport(), badge: badgeReport(),
                    counts: countsReport(), bar: barReport() }));
            } catch (e) {
                console.warn("RESTYLE_F_ERROR " + String(e));
            }
            Qt.quit();
        }
    }
    Timer { interval: 30000; running: true; onTriggered: Qt.quit() }
}
"##;

fn run_probe() -> serde_json::Value {
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
                &QUrl::from("qrc:/inline_restyle_f_probe.qml"),
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
    assert!(!captured.contains("RESTYLE_F_ERROR"), "{captured}");
    assert!(!captured.contains("Error:"), "QML error: {captured}");
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    serde_json::from_str(&line).expect("probe JSON")
}

fn all_true(v: &serde_json::Value) -> bool {
    v.as_array()
        .unwrap()
        .iter()
        .all(|b| b.as_bool() == Some(true))
}
fn all_false(v: &serde_json::Value) -> bool {
    v.as_array()
        .unwrap()
        .iter()
        .all(|b| b.as_bool() == Some(false))
}

#[test]
fn restyle_batch_f_geometry_and_focus() {
    let v = run_probe();
    let hit = v["hitMin"].as_f64().unwrap();
    assert!(hit >= 44.0, "Theme.hitMin: {v}");

    // MapMarker: hit region >= hitMin each way, visual size unchanged.
    let markers = v["markers"].as_array().unwrap();
    assert_eq!(markers.len(), 8, "{v}");
    let mut any_small = false;
    for m in markers {
        let (w, h) = (m["w"].as_f64().unwrap(), m["h"].as_f64().unwrap());
        let expect = m["diameter"].as_f64().unwrap() + 2.0 * m["small"].as_f64().unwrap();
        assert!(
            (w - expect).abs() < 0.01 && (h - expect).abs() < 0.01,
            "visual size changed: {m}"
        );
        any_small |= w < hit;
        assert!(
            all_true(&m["cornersIn"]),
            "hit region corners under hitMin: {m}"
        );
        assert!(
            all_true(&m["sidesIn"]),
            "hit region sides under hitMin: {m}"
        );
        assert!(
            all_true(&m["childAtIn"]),
            "scene hit test misses the extension: {m}"
        );
        assert!(
            all_false(&m["outside"]),
            "hit region far larger than needed: {m}"
        );
        assert!(
            all_false(&m["childAtOut"]),
            "scene hit test beyond the region: {m}"
        );
        assert_eq!(m["focusable"], true, "{m}");
        assert_eq!(m["role"].as_i64(), Some(43), "marker role is Button: {m}");
        // The focus halo is drawn outside the verdict shape and never has its colour.
        assert_eq!(m["haloCount"].as_i64(), Some(1), "{m}");
        assert_eq!(
            m["haloIsVerdict"], false,
            "halo equals the verdict fill: {m}"
        );
    }
    assert!(
        any_small,
        "fixture must include a marker whose visual is under hitMin: {v}"
    );

    // Badge: capsule, small, no focus.
    let b = &v["badge"];
    assert_eq!(b["surfaces"].as_i64(), Some(1), "{b}");
    assert_eq!(b["pill"], true, "{b}");
    assert!(
        b["h"].as_f64().unwrap() < hit,
        "badge must not be inflated to hitMin: {b}"
    );
    assert!(b["radius"].as_f64().unwrap() > 0.0, "{b}");
    assert_eq!(b["tab"], false, "{b}");
    assert_eq!(b["focusPolicy"].as_i64(), Some(0), "{b}");

    // Verdict-count chips: capsules, small, no Tab stop.
    let c = &v["counts"];
    assert!(c["surfaces"].as_i64().unwrap() >= 2, "{c}");
    assert_eq!(c["allPill"], true, "{c}");
    assert!(
        c["maxH"].as_f64().unwrap() < hit,
        "chips must not be inflated: {c}"
    );
    assert_eq!(c["tabs"].as_i64(), Some(0), "{c}");

    // BackgroundGroupBar buttons: >= hitMin, restyled in place.
    for b in v["bar"].as_array().unwrap() {
        assert_eq!(b["found"], true, "{b}");
        assert!(b["h"].as_f64().unwrap() >= hit, "button under hitMin: {b}");
        assert!(
            b["w"].as_f64().unwrap() >= b["iw"].as_f64().unwrap() - 0.01,
            "label cut: {b}"
        );
        // The style's own Button (what the keyboard probes name `Button`), not Rounded*/Guarded*.
        assert!(
            b["type"].as_str().unwrap().starts_with("Button_QMLTYPE"),
            "type name changed: {b}"
        );
        assert_eq!(b["surfaces"].as_i64(), Some(1), "{b}");
        assert_eq!(b["pill"], true, "{b}");
    }
}

fn qml_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("qml")
        .join(format!("{name}.qml"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn code_of(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Palette-only colours: no hex, no rgb()/hsl() text, no colour constructor, no
/// quoted colour name (only "transparent") on a colour property.
fn colour_violations(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (n, line) in code.lines().enumerate() {
        let t = line.trim();
        let hex = t.split('#').skip(1).any(|r| {
            let h: String = r.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            matches!(h.len(), 3 | 4 | 6 | 8)
        });
        let ctor = [
            "Qt.rgba(",
            "Qt.color(",
            "Qt.hsla(",
            "Qt.hsva(",
            "rgb(",
            "hsl(",
        ]
        .iter()
        .any(|f| t.contains(f));
        let named = (t.starts_with("color:") || t.starts_with("border.color:"))
            && t.contains(['"', '\''])
            && !t.contains("\"transparent\"");
        if hex || ctor || named {
            out.push(format!("{n}: {t}"));
        }
    }
    out
}

#[test]
fn the_colour_lint_rejects_literals_and_allows_the_palette() {
    for bad in [
        "color: \"#ff0000\"",
        "color: \"red\"",
        "border.color: Qt.rgba(1,0,0,1)",
        "x: \"#abc\"",
    ] {
        assert!(!colour_violations(bad).is_empty(), "should reject {bad}");
    }
    for ok in [
        "color: \"transparent\"",
        "color: tip.palette.toolTipBase",
        "color: Qt.alpha(a.palette.windowText, 0.1)",
    ] {
        assert!(colour_violations(ok).is_empty(), "should allow {ok}");
    }
}

#[test]
fn batch_f_sources_use_theme_radii_palette_colours_and_no_input_area() {
    let files = [
        "GroupPendingBadge",
        "GroupVerdictCounts",
        "HoverTip",
        "BackgroundGroupBar",
        "MapMarker",
        "LearningBanner",
    ];
    for name in files {
        let code = code_of(&qml_source(name));
        assert!(
            colour_violations(&code).is_empty(),
            "{name}: {:?}",
            colour_violations(&code)
        );
        assert!(!code.contains("MouseArea"), "{name}: MouseArea");
        assert!(!code.contains("TapHandler"), "{name}: TapHandler");
    }
    let badge = code_of(&qml_source("GroupPendingBadge"));
    assert!(badge.contains("RoundedSurface"), "badge: RoundedSurface");
    let counts = code_of(&qml_source("GroupVerdictCounts"));
    assert!(counts.contains("RoundedSurface"), "counts: RoundedSurface");
    // Capsules come from RoundedSurface's default (`pill: true` = radiusPill).
    let surface = code_of(&qml_source("RoundedSurface"));
    assert!(
        surface.contains("pill ? Theme.radiusPill"),
        "RoundedSurface pill radius"
    );
    for (name, code) in [("badge", &badge), ("counts", &counts)] {
        assert!(!code.contains("pill: false"), "{name}: must stay a capsule");
    }
    let tip = code_of(&qml_source("HoverTip"));
    assert!(
        tip.contains("radius: Theme.radiusSmall"),
        "HoverTip radiusSmall"
    );
    let marker = code_of(&qml_source("MapMarker"));
    assert!(marker.contains("containmentMask"), "MapMarker hit area");
    assert!(
        marker.contains("Theme.hitMin"),
        "MapMarker hit area uses hitMin"
    );
    assert!(
        marker.contains("objectName: \"focusHalo\""),
        "MapMarker focus halo"
    );
    let bar = code_of(&qml_source("BackgroundGroupBar"));
    assert_eq!(
        bar.matches("background: RoundedSurface").count(),
        4,
        "bar buttons"
    );
    assert_eq!(bar.matches("Theme.hitMin").count(), 4, "bar buttons hitMin");
    // LearningBanner: Kirigami draws the InlineMessage; its buttons are GuardedButton.
    let banner = code_of(&qml_source("LearningBanner"));
    assert!(banner.contains("Kirigami.InlineMessage"), "banner");
    assert!(
        !banner.contains("Controls.Button"),
        "banner: plain buttons would need restyling"
    );
    // The documented non-tappable decision stays written down.
    for name in ["GroupPendingBadge", "GroupVerdictCounts"] {
        let src = qml_source(name);
        assert!(
            src.contains("NOT independently tappable"),
            "{name}: missing the not-tappable note"
        );
        assert!(
            src.contains("Theme.hitMin"),
            "{name}: note must name the row's hit minimum"
        );
    }
}
