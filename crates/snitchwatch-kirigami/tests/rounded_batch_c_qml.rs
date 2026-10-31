//! Restyle batch C (Status pages): the two new components (`RoundedCheckBox`,
//! `RoundedComboBox`) and the swapped sites on the batch's pages.
//!
//! Probe: on a compact-style fixture (no padding, 8 px font) only the clamp in
//! the component can keep it at `Theme.hitMin` in both directions; the ring
//! shows for keyboard focus only and contrasts with the fill it sits on; the
//! visuals add no Tab stop; a swapped site on a real page is >= 44 px.
//! Source guards: exactly which plain controls remain in each of the batch's
//! eight files, and that the selector keeps its held-key guard.
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::about_model as _;
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
mod common;
mod qml_guard_support;
use common::{capture_stderr, init_headless_qt_env};
use qml_guard_support::code_lines;

const MARKER: &str = "ROUNDED_BATCH_C_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000; height: 900
    property var fixture: null
    property var pages: []
    property int ticks: 0
    property bool focusDone: false
    property var focusResult: []
    property var mirrors: []
    readonly property var interactive: [Accessible.Button, Accessible.CheckBox,
        Accessible.RadioButton, Accessible.ComboBox]

    function walk(root, out, seen) {
        if (!root || seen.has(root)) return out;
        seen.add(root);
        out.push(root);
        const kids = [];
        if (root.children) for (let i = 0; i < root.children.length; ++i) kids.push(root.children[i]);
        if (root.contentItem && root.contentItem !== root) kids.push(root.contentItem);
        if (root.background) kids.push(root.background);
        if (root.indicator) kids.push(root.indicator);
        for (const k of kids) walk(k, out, seen);
        return out;
    }
    function drawn(i) { return i.visible && i.width > 0 && i.height > 0; }
    function controlsOf(root) {
        return walk(root, [], new Set()).filter(i => drawn(i) && probe.interactive.indexOf(i.Accessible.role) >= 0
            && i.objectName !== "denyWarning");
    }
    function surfaceOf(c) {
        return walk(c, [], new Set()).filter(i => i.focusRing !== undefined && i.cornerRadius !== undefined
            && i.control === c)[0];
    }
    function describe(c) {
        return { name: c.Accessible.name || c.objectName || c.text || "?", w: c.width, h: c.height,
                 iw: c.implicitWidth, ih: c.implicitHeight };
    }
    function tabStops(root) {
        return walk(root, [], new Set()).filter(i => i.activeFocusOnTab === true && drawn(i)).length;
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
    function ringContrast(s) {
        const out = [];
        for (const wash of [0, Theme.hoverOpacity, Theme.pressedOpacity]) {
            const base = s.body.visible ? over(s.body.color, s.control.palette.window) : s.control.palette.window;
            const w = s.wash.color;
            const fill = over(Qt.rgba(w.r, w.g, w.b, wash), base);
            out.push(ratio(over(s.focusRing.border.color, fill), fill));
        }
        return out;
    }
    function effectiveOpacity(item, stopAt) {
        let o = 1;
        for (let i = item; i && i !== stopAt.parent; i = i.parent) o *= i.opacity;
        return o;
    }
    function overlaps(a, b) {
        return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
    }
    // Where the selected text (its painted extent) and the chevron / box sit, in the control's space.
    function mirrorRow(rtl, kind, c) {
        const text = c.contentItem;
        const mark = c.indicator;
        const t = text.mapToItem(c, 0, 0);
        const m = mark.mapToItem(c, 0, 0);
        // The Text's own padding (the box's side) is not part of where the glyphs go.
        const avail = text.width - text.leftPadding - text.rightPadding;
        const textRect = { x: t.x + text.leftPadding, y: t.y, width: Math.min(text.paintedWidth, avail), height: text.height };
        const markRect = { x: m.x, y: m.y, width: mark.width, height: mark.height };
        return { rtl: rtl, kind: kind, mirrored: c.mirrored, overlap: overlaps(textRect, markRect),
                 elided: text.paintedWidth > 0 && text.truncated, w: c.width,
                 textRight: textRect.x + textRect.width, markLeft: markRect.x, textLeft: textRect.x,
                 markRight: markRect.x + markRect.width,
                 textInside: textRect.x >= -0.5 && textRect.x + textRect.width <= c.width + 0.5 };
    }
    function mirrorChecks() {
        const out = [];
        for (const m of mirrors) out.push(mirrorRow(m.rtl, "combo", m.combo), mirrorRow(m.rtl, "check", m.check));
        return out;
    }
    function focusChecks(all) {
        const out = [];
        for (const c of all) {
            if (!c.enabled) continue;
            c.forceActiveFocus(Qt.TabFocusReason);
            const s = surfaceOf(c);
            const on = !!s && s.focusRing.visible && c.visualFocus;
            const contrast = s ? ringContrast(s) : [-1];
            const prominentClash = !!s && s.prominent
                && Qt.colorEqual(s.focusRing.border.color, s.control.palette.highlight);
            const ringBorder = s ? s.focusRing.border.width : -1;
            const ringOpacity = s ? effectiveOpacity(s.focusRing, c) * s.focusRing.border.color.a : -1;
            c.focus = false;
            c.forceActiveFocus(Qt.MouseFocusReason);
            out.push({ name: describe(c).name, on: on, ringBorder: ringBorder, ringOpacity: ringOpacity, contrast: contrast, prominentClash: prominentClash,
                       radiusOk: !!s && s.body.radius === s.focusRing.radius,
                       mouseOff: !!s && !s.focusRing.visible });
        }
        return out;
    }
    function report(state) {
        const fixtureControls = controlsOf(fixture);
        const pageControls = pages.map(p => controlsOf(p).map(describe));
        console.warn("ROUNDED_BATCH_C_RESULT " + JSON.stringify({
            state: state, hitMin: Theme.hitMin, focusRingWidth: Theme.focusRingWidth,
            fixture: fixtureControls.map(describe),
            fixtureTabStops: tabStops(fixture),
            pages: pageControls,
            focus: focusResult,
            mirror: mirrorChecks()
        }));
        Qt.quit();
    }
    Component.onCompleted: {
        const make = (url, props) => {
            const c = Qt.createComponent(url);
            if (c.status !== Component.Ready) throw new Error(c.errorString());
            return c.createObject(contentItem, props);
        };
        fixture = fixtureComp.createObject(contentItem, { x: 0, y: 0, width: 900 });
        mirrors = [false, true].map(rtl => mirrorComp.createObject(contentItem, { x: 0, y: 820, rtl: rtl }));
        mirrors.forEach((m, i) => { m.x = i * 450; });
        const base = "qrc:/qt/qml/com/snitchwatch/shell/qml/";
        pages = [
            make(base + "DiagnosticsPage.qml", { x: 0, y: 100, width: 600, height: 700 })
        ];
    }
    // The same long text in both directions (LayoutMirroring is inherited by the children).
    Component {
        id: mirrorComp
        Item {
            property bool rtl: false
            property alias combo: mirrorCombo
            property alias check: mirrorCheck
            width: 400; height: 100
            LayoutMirroring.enabled: rtl
            LayoutMirroring.childrenInherit: true
            RoundedComboBox { id: mirrorCombo; width: 180; model: ["A very long selected choice that cannot fit"] }
            RoundedCheckBox { id: mirrorCheck; y: 50; width: 180; text: "A very long check box label that cannot fit" }
        }
    }
    Component {
        id: fixtureComp
        RowLayout {
            spacing: 12
            // Compact style: nothing but the component's own clamp gives 44 px.
            RoundedCheckBox { objectName: "cbBare"; Accessible.name: "Bare check"; font.pixelSize: 8
                padding: 0; topPadding: 0; bottomPadding: 0; leftPadding: 0; rightPadding: 0
                topInset: 0; bottomInset: 0; leftInset: 0; rightInset: 0 }
            RoundedCheckBox { objectName: "cbText"; text: "Check"; Accessible.name: "Check"; font.pixelSize: 8
                padding: 0; topPadding: 0; bottomPadding: 0; leftPadding: 0; rightPadding: 0
                topInset: 0; bottomInset: 0; leftInset: 0; rightInset: 0 }
            RoundedCheckBox { objectName: "cbOn"; text: "On"; checked: true; Accessible.name: "On"; font.pixelSize: 8 }
            RoundedComboBox { objectName: "combo"; model: ["a", "b"]; Accessible.name: "Combo"; font.pixelSize: 8
                padding: 0; topPadding: 0; bottomPadding: 0; leftPadding: 0; rightPadding: 0
                topInset: 0; bottomInset: 0; leftInset: 0; rightInset: 0 }
            RoundedComboBox { objectName: "comboDark"; model: ["Dark choice"]; Accessible.name: "Dark combo"
                palette.window: "#202326"; palette.windowText: "#eff0f1"; palette.buttonText: "#eff0f1"
                palette.highlight: "#3daee9"; palette.highlightedText: "#ffffff" }
            RoundedCheckBox { objectName: "cbDark"; text: "Dark"; checked: true; Accessible.name: "Dark check"
                palette.window: "#202326"; palette.windowText: "#eff0f1"; palette.buttonText: "#eff0f1"
                palette.highlight: "#3daee9"; palette.highlightedText: "#ffffff" }
        }
    }
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            ticks += 1;
            try {
                if (!fixture || pages.length < 1) return;
                if (controlsOf(fixture).length < 6 || controlsOf(pages[0]).length < 4) return;
                if (!focusDone) {
                    focusDone = true;
                    probe.requestActivate();
                    focusResult = focusChecks(controlsOf(fixture));
                    return;
                }
                report("ready");
            } catch (e) {
                console.warn("ROUNDED_BATCH_C_ERROR " + String(e));
                Qt.quit();
            }
        }
    }
    Timer { interval: 45000; running: true; onTriggered: report("timeout") }
}
"##;

fn run_probe() -> serde_json::Value {
    init_headless_qt_env();
    let mut app = QGuiApplication::new();
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
                &QByteArray::from(QML),
                &QUrl::from("qrc:/inline_rounded_batch_c_probe.qml"),
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
    assert!(!captured.contains("ROUNDED_BATCH_C_ERROR"), "{captured}");
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    serde_json::from_str(&line).expect("probe JSON")
}

fn assert_hit(c: &serde_json::Value, line: &str) {
    let (w, h) = (c["w"].as_f64().unwrap(), c["h"].as_f64().unwrap());
    assert!(w >= 44.0 && h >= 44.0, "hit box under 44 px: {c} in {line}");
    assert!(
        w + 0.5 >= c["iw"].as_f64().unwrap(),
        "narrower than its content wants (truncates): {c}"
    );
}

#[test]
fn the_new_components_and_swapped_sites_are_big_rounded_and_show_a_focus_ring() {
    let v = run_probe();
    let line = v.to_string();
    assert_eq!(v["state"], "ready", "not ready in time: {line}");
    assert!(v["hitMin"].as_f64().unwrap() >= 44.0, "Theme.hitMin: {line}");

    let fixture = v["fixture"].as_array().unwrap();
    assert_eq!(fixture.len(), 6, "fixture controls: {line}");
    for c in fixture {
        assert_hit(c, &line);
    }
    // The visuals (box, tick, chevron, surface) are role-less: no extra stop.
    assert_eq!(v["fixtureTabStops"], 6, "extra Tab stops: {line}");

    // Diagnostics (with no controller): 5 geo buttons, 2 switches, 2 selectors,
    // Check again, Refresh. Daemon Health / Deny status need a live model, so
    // their two swapped buttons are covered by the source guard below and by
    // the drawer keyboard probe.
    let diagnostics = v["pages"][0].as_array().unwrap();
    assert_eq!(diagnostics.len(), 11, "Diagnostics controls: {line}");
    for c in diagnostics {
        assert_hit(c, &line);
    }
    for name in ["Check again", "Refresh", "Allow remembers for: Once"] {
        assert!(
            diagnostics.iter().any(|c| c["name"] == name),
            "{name} missing: {line}"
        );
    }

    let focus = v["focus"].as_array().unwrap();
    assert_eq!(focus.len(), 6, "focus checks: {line}");
    for f in focus {
        assert_eq!(f["on"], true, "no focus ring on keyboard focus: {f}");
        assert_eq!(f["mouseOff"], true, "focus ring shown for mouse focus: {f}");
        assert_eq!(f["radiusOk"], true, "ring radius differs from the body: {f}");
        assert_eq!(f["prominentClash"], false, "ring equals a prominent fill: {f}");
        // A zero-width or transparent ring would pass the checks above.
        assert_eq!(f["ringBorder"], v["focusRingWidth"], "ring width is not Theme.focusRingWidth: {f}");
        assert!(f["ringBorder"].as_f64().unwrap() > 0.0, "focus ring has no width: {f}");
        assert!(
            f["ringOpacity"].as_f64().unwrap() > 0.5,
            "focus ring is (nearly) transparent: {f}"
        );
        assert!(
            f["contrast"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c.as_f64().unwrap() >= 2.0),
            "focus ring does not contrast with its fill: {f}"
        );
    }
    // Long text, both directions: the text must clear the chevron / box.
    let rows = v["mirror"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "mirror rows: {line}");
    for r in rows {
        assert_eq!(r["mirrored"], r["rtl"], "LayoutMirroring did not reach the control: {r}");
        assert_eq!(r["elided"], true, "text was meant to be too long to fit: {r}");
        assert_eq!(r["overlap"], false, "text overlaps the chevron / box: {r}");
        assert_eq!(r["textInside"], true, "text leaves the control: {r}");
    }
}

fn qml_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("qml")
        .join(format!("{name}.qml"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Count `needle` at the start of a (trimmed) code line, so `Controls.Button`
/// does not match `GuardedButton`.
fn sites(code: &str, needle: &str) -> usize {
    code.lines().filter(|l| l.trim_start().starts_with(needle)).count()
}

/// (file, plain controls left: (Controls.Button, Controls.Switch, RadioButton,
/// ToolButton, CheckBox, ComboBox), swapped: (RoundedButton, RoundedComboBox)).
/// Everything else the batch's files hold of these kinds is a `Guarded*`
/// wrapper. Exceptions: none; GeoPage and TrafficPage hold no such control.
const BATCH: &[(&str, [usize; 6], [usize; 2])] = &[
    ("DiagnosticsPage.qml", [0; 6], [2, 2]),
    ("GeoPage.qml", [0; 6], [0, 0]),
    ("GeoDatabaseSettings.qml", [0; 6], [5, 0]),
    ("DaemonHealthPage.qml", [0; 6], [1, 0]),
    ("DenyStatusSection.qml", [0; 6], [1, 0]),
    ("ScannerPage.qml", [0; 6], [0, 0]),
    ("SilentModeChoice.qml", [0; 6], [0, 0]),
    ("TrafficPage.qml", [0; 6], [0, 0]),
];

#[test]
fn the_batch_files_keep_no_plain_swappable_control() {
    for (name, plain, swapped) in BATCH {
        let code = code_lines(&qml_source(name.trim_end_matches(".qml")));
        let kinds = [
            "Controls.Button {",
            "Controls.Switch {",
            "Controls.RadioButton {",
            "Controls.ToolButton {",
            "Controls.CheckBox {",
            "Controls.ComboBox {",
        ];
        for (kind, want) in kinds.iter().zip(plain) {
            assert_eq!(sites(&code, kind), *want, "{name}: plain `{kind}` left");
        }
        assert_eq!(sites(&code, "RoundedButton {"), swapped[0], "{name}: RoundedButton sites");
        assert_eq!(sites(&code, "RoundedComboBox {"), swapped[1], "{name}: RoundedComboBox sites");
    }
}

#[test]
fn the_selector_keeps_the_held_key_guard_and_the_sites_do_not_repeat_it() {
    let combo = code_lines(&qml_source("RoundedComboBox"));
    for needle in [
        "Keys.priority: Keys.BeforeItem",
        "Keys.onPressed: event => { if (event.isAutoRepeat) event.accepted = true; }",
        "Keys.onReleased: event => { if (event.isAutoRepeat) event.accepted = true; }",
    ] {
        assert!(combo.contains(needle), "RoundedComboBox lost `{needle}`");
    }
    let page = code_lines(&qml_source("DiagnosticsPage"));
    assert!(!page.contains("Keys.onPressed"), "DiagnosticsPage repeats the guard");
}

#[test]
fn the_new_components_add_no_input_area_and_no_literal_radius() {
    for name in ["RoundedCheckBox", "RoundedComboBox"] {
        let code = code_lines(&qml_source(name));
        for (n, line) in code.lines().enumerate() {
            let t = line.trim();
            assert!(
                !t.starts_with("MouseArea") && !t.starts_with("TapHandler"),
                "{name}:{n}: input of its own: {t}"
            );
            assert!(
                !(t.starts_with("radius:")
                    && t["radius:".len()..].trim().starts_with(|c: char| c.is_ascii_digit())),
                "{name}:{n}: literal radius: {t}"
            );
        }
    }
}
