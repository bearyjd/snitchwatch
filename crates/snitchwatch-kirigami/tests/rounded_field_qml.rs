//! `RoundedField` (single-line) and `RoundedTextArea` (multi-line), the rounded
//! text boxes of restyle batch D. Asserted on a live item tree, not on source:
//! the hit box is at least `Theme.hitMin` high even when the desktop style would
//! make it shorter (compact-style fixture), the focus ring shows on keyboard AND
//! mouse focus, is wider than 0, effectively opaque (> 0.5), contrasts >= 2.0
//! with the fill in a light and a dark palette and never equals that fill, the
//! frame adds no Tab stop, and under a mirrored (right-to-left) layout the text
//! sits at the right edge inside symmetric padding. Run headless with
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

const MARKER: &str = "ROUNDED_FIELD_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 700; height: 600

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
    function effOpacity(item) {
        let o = 1;
        for (let p = item; p; p = p.parent) o *= p.opacity;
        return o;
    }
    function walk(root, out, seen) {
        if (!root || seen.has(root)) return out;
        seen.add(root); out.push(root);
        const kids = [];
        if (root.children) for (let i = 0; i < root.children.length; ++i) kids.push(root.children[i]);
        if (root.contentItem && root.contentItem !== root) kids.push(root.contentItem);
        if (root.background) kids.push(root.background);
        for (const k of kids) walk(k, out, seen);
        return out;
    }
    function tabStops(root) {
        return walk(root, [], new Set()).filter(i => i.activeFocusOnTab === true && i.visible).length;
    }
    function frameOf(c) { return c.background; }
    function ringFacts(c) {
        const f = frameOf(c);
        const fill = over(f.body.color, c.palette.window);
        const ringOn = over(f.focusRing.border.color, fill);
        return { visible: f.focusRing.visible, width: f.focusRing.border.width,
                 eff: effOpacity(f.focusRing) * f.focusRing.border.color.a,
                 contrast: ratio(ringOn, fill),
                 equalsFill: Qt.colorEqual(f.focusRing.border.color, f.body.color),
                 radius: f.body.radius, ringRadius: f.focusRing.radius,
                 radiusOk: f.body.radius === Theme.radiusSmall && f.focusRing.radius === Theme.radiusSmall };
    }
    function focusFacts(c) {
        const out = {};
        c.focus = false;
        out.before = ringFacts(c).visible;
        c.forceActiveFocus(Qt.TabFocusReason);
        out.tab = ringFacts(c);
        c.focus = false;
        c.forceActiveFocus(Qt.MouseFocusReason);
        out.mouse = ringFacts(c);
        c.focus = false;
        out.after = ringFacts(c).visible;
        return out;
    }

    Column {
        id: col
        spacing: 8
        width: 400
        RoundedField { id: field; width: 300; text: "hello"; Accessible.name: "Plain field"
            palette.window: "#ffffff"; palette.windowText: "#202326"; palette.base: "#fcfcfc"
            palette.text: "#202326"; palette.highlight: "#308cc6" }
        RoundedField { id: empty; placeholderText: "Placeholder"; Accessible.name: "Empty field" }
        RoundedField { id: dark; width: 300; text: "dark"; Accessible.name: "Dark field"
            palette.window: "#202326"; palette.windowText: "#eff0f1"; palette.base: "#1b1e20"
            palette.text: "#eff0f1"; palette.highlight: "#3daee9" }
        RoundedTextArea { id: area; width: 300; text: "one"; Accessible.name: "Area"
            palette.window: "#ffffff"; palette.windowText: "#202326"; palette.base: "#fcfcfc"
            palette.text: "#202326"; palette.highlight: "#308cc6" }
        // A compact style: no padding, tiny font, a 10 px background. Only the
        // clamp in the component can keep this at Theme.hitMin.
        RoundedField {
            id: compact; width: 300; text: "x"
            padding: 0; topPadding: 0; bottomPadding: 0; leftPadding: 0; rightPadding: 0
            topInset: 0; bottomInset: 0; font.pixelSize: 8
            background: Item { implicitWidth: 10; implicitHeight: 10 }
        }
        RoundedTextArea {
            id: compactArea; width: 300; text: "x"
            padding: 0; topPadding: 0; bottomPadding: 0; leftPadding: 0; rightPadding: 0
            font.pixelSize: 8
            background: Item { implicitWidth: 10; implicitHeight: 10 }
        }
    }
    Item {
        id: rtl
        y: 450; width: 400; height: 100
        LayoutMirroring.enabled: true
        LayoutMirroring.childrenInherit: true
        RoundedField { id: rtlField; width: 300; text: "\u0645\u0631\u062d\u0628\u0627"; Accessible.name: "Mirrored field" }
        RoundedTextArea { id: rtlArea; y: 50; width: 200; wrapMode: TextEdit.Wrap; Accessible.name: "Mirrored area"
            text: "\u0645\u0631\u062d\u0628\u0627 \u0628\u0627\u0644\u0639\u0627\u0644\u0645 \u0645\u0631\u062d\u0628\u0627 \u0628\u0627\u0644\u0639\u0627\u0644\u0645 \u0645\u0631\u062d\u0628\u0627 \u0628\u0627\u0644\u0639\u0627\u0644\u0645 \u0645\u0631\u062d\u0628\u0627 \u0628\u0627\u0644\u0639\u0627\u0644\u0645" }
    }
    Item {
        id: ltr
        y: 450; x: 400; width: 250; height: 150
        LayoutMirroring.enabled: false
        LayoutMirroring.childrenInherit: true
        RoundedTextArea { id: ltrArea; width: 200; wrapMode: TextEdit.Wrap; Accessible.name: "LTR area"
            text: "The quick brown fox jumps over the lazy dog and keeps running across the field" }
        RoundedField { id: ltrField; y: 160; width: 200; text: "hello"; Accessible.name: "LTR field" }
    }

    function areaFacts(a) {
        let minX = 1e9, maxX = -1e9;
        for (let i = 0; i <= a.text.length; ++i) {
            const r = a.positionToRectangle(i);
            minX = Math.min(minX, r.x); maxX = Math.max(maxX, r.x);
        }
        const p0 = a.positionToRectangle(0);
        return { token: Theme.spacingMedium, p0x: p0.x, align: a.effectiveHorizontalAlignment, mirrored: a.LayoutMirroring.enabled, w: a.width, h: a.height, left: a.leftPadding,
                 right: a.rightPadding, minX: minX, maxX: maxX, lines: a.lineCount,
                 contentW: a.contentWidth, contentH: a.contentHeight,
                 top: a.topPadding, bottom: a.bottomPadding, code0: a.text.charCodeAt(0),
                 bodyW: a.background.body.width, bodyX: a.background.body.x };
    }

    function report() {
        try {
            const sized = {};
            for (const [k, c] of Object.entries({ field: field, empty: empty, dark: dark, area: area,
                                                  compact: compact, compactArea: compactArea,
                                                  rtlArea: rtlArea, ltrArea: ltrArea })) {
                sized[k] = { h: c.height, ih: c.implicitHeight, w: c.width, iw: c.implicitWidth };
            }
            const r0 = rtlField.positionToRectangle(0);
            const r1 = rtlField.positionToRectangle(rtlField.text.length);
            console.warn("ROUNDED_FIELD_RESULT " + JSON.stringify({
                hitMin: Theme.hitMin, sized: sized,
                focus: { field: focusFacts(field), dark: focusFacts(dark), area: focusFacts(area),
                         rtlArea: focusFacts(rtlArea), ltrArea: focusFacts(ltrArea) },
                tabStops: { field: tabStops(field), area: tabStops(area), rtlArea: tabStops(rtlArea), ltrArea: tabStops(ltrArea), rtl: tabStops(rtlField) },
                rtl: { code0: rtlField.text.charCodeAt(0), mirrored: rtlField.LayoutMirroring.enabled, w: rtlField.width, left: rtlField.leftPadding,
                       right: rtlField.rightPadding, x0: r0.x, x1: r1.x, align: rtlField.effectiveHorizontalAlignment,
                       ltrX0: ltrField.positionToRectangle(0).x,
                       ltrAlign: ltrField.effectiveHorizontalAlignment,
                       bodyW: rtlField.background.body.width, bodyX: rtlField.background.body.x },
                areas: { rtl: areaFacts(rtlArea), ltr: areaFacts(ltrArea) },
                dark: { highlight: dark.palette.highlight.toString(), base: dark.palette.base.toString() }
            }));
        } catch (e) {
            console.warn("ROUNDED_FIELD_ERROR " + String(e));
        }
        Qt.quit();
    }
    Timer { interval: 400; running: true; onTriggered: probe.report() }
    Timer { interval: 20000; running: true; onTriggered: Qt.quit() }
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
                &QUrl::from("qrc:/inline_rounded_field_probe.qml"),
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
    assert!(!captured.contains("ROUNDED_FIELD_ERROR"), "{captured}");
    assert!(!captured.contains("Error:"), "QML error: {captured}");
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    serde_json::from_str(&line).expect("probe JSON")
}

fn num(v: &serde_json::Value, ctx: &str) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {ctx}"))
}

#[test]
fn rounded_fields_hit_box_focus_ring_tab_stops_and_mirroring() {
    let v = run_probe();
    let line = v.to_string();
    let hit = num(&v["hitMin"], &line);
    assert!(hit >= 44.0, "Theme.hitMin: {line}");

    // Hit box: every field and area, including the compact-style fixtures.
    for (name, s) in v["sized"].as_object().unwrap() {
        assert!(
            num(&s["h"], &line) >= hit,
            "{name} under Theme.hitMin high: {line}"
        );
        assert!(
            num(&s["ih"], &line) >= hit,
            "{name} implicit height: {line}"
        );
        // A field with no width of its own must not collapse to a 44 px stub.
        // (The compact fixtures replace the frame, so they have none to give.)
        assert!(
            name.starts_with("compact") || num(&s["iw"], &line) >= 150.0,
            "{name} implicit width: {line}"
        );
    }

    // Focus ring: keyboard and mouse focus, both palettes, field and area.
    for (name, f) in v["focus"].as_object().unwrap() {
        assert_eq!(f["before"], false, "{name}: ring without focus: {line}");
        assert_eq!(
            f["after"], false,
            "{name}: ring stuck after focus left: {line}"
        );
        for how in ["tab", "mouse"] {
            let r = &f[how];
            assert_eq!(r["visible"], true, "{name}/{how}: no focus ring: {line}");
            assert!(
                num(&r["width"], &line) > 0.0,
                "{name}/{how}: ring width 0: {line}"
            );
            assert!(
                num(&r["eff"], &line) > 0.5,
                "{name}/{how}: ring effectively transparent: {line}"
            );
            assert!(
                num(&r["contrast"], &line) >= 2.0,
                "{name}/{how}: ring contrast under 2.0: {line}"
            );
            assert_eq!(
                r["equalsFill"], false,
                "{name}/{how}: ring equals its fill: {line}"
            );
            assert_eq!(
                r["radiusOk"], true,
                "{name}/{how}: radius not Theme.radiusSmall: {line}"
            );
        }
    }

    // The frame adds no Tab stop: one per field.
    for (name, n) in v["tabStops"].as_object().unwrap() {
        assert_eq!(n, 1, "{name}: Tab stops: {line}");
    }

    // Mirrored layout: symmetric padding, text at the right edge and inside it,
    // frame still covers the whole field.
    let r = &v["rtl"];
    assert_eq!(r["mirrored"], true, "layout not mirrored: {line}");
    assert_eq!(
        r["code0"], 0x0645,
        "the mirrored fixture is not Arabic text: {line}"
    );
    let (w, l, rp) = (
        num(&r["w"], &line),
        num(&r["left"], &line),
        num(&r["right"], &line),
    );
    assert!(
        (l - rp).abs() < 0.01 && l > 0.0,
        "padding not symmetric: {line}"
    );
    let (x0, x1) = (num(&r["x0"], &line), num(&r["x1"], &line));
    assert!(
        x0 > w / 2.0,
        "right-to-left text not at the right edge: {line}"
    );
    assert!(
        x0 <= w - rp + 0.5 && x1 >= l - 0.5,
        "text outside the padding: {line}"
    );
    // Alignment: the mirrored field's text starts on the RIGHT padded edge
    // (not centred), the LTR field's on the LEFT padded edge.
    const ALIGN_LEFT: f64 = 1.0;
    const ALIGN_RIGHT: f64 = 2.0;
    assert_eq!(
        num(&r["align"], &line),
        ALIGN_RIGHT,
        "mirrored field not right-aligned: {line}"
    );
    // TextField cursor rectangles are in content coordinates (origin at the left padding).
    assert!(
        (x0 - (w - l - rp)).abs() < 2.0,
        "mirrored field text not on the right padded edge: {line}"
    );
    assert_eq!(
        num(&r["ltrAlign"], &line),
        ALIGN_LEFT,
        "LTR field not left-aligned: {line}"
    );
    assert!(
        num(&r["ltrX0"], &line).abs() < 2.0,
        "LTR field text not on the left padded edge: {line}"
    );
    assert!(
        (num(&r["bodyW"], &line) - w).abs() < 0.5 && num(&r["bodyX"], &line).abs() < 0.5,
        "frame does not cover the mirrored field: {line}"
    );

    // Multi-line area, mirrored and not: symmetric padding at least the token,
    // wrapped text inside the padding, a hit-high control, a frame over all of it.
    for (name, rtl) in [("ltr", false), ("rtl", true)] {
        let a = &v["areas"][name];
        assert_eq!(a["mirrored"], rtl, "{name} area mirroring: {line}");
        if rtl {
            assert_eq!(
                a["code0"], 0x0645,
                "the mirrored area is not Arabic text: {line}"
            );
        }
        let (w, l, rp) = (
            num(&a["w"], &line),
            num(&a["left"], &line),
            num(&a["right"], &line),
        );
        assert!(
            (l - rp).abs() < 0.01,
            "{name} area padding not symmetric: {line}"
        );
        let tok = num(&a["token"], &line);
        assert!(
            tok > 0.0 && l >= tok && rp >= tok,
            "{name} area padding under the token: {line}"
        );
        assert!(
            num(&a["lines"], &line) > 1.0,
            "{name} area text did not wrap: {line}"
        );
        assert!(
            num(&a["contentW"], &line) <= w - l - rp + 0.5,
            "{name} area content wider than the padded box: {line}"
        );
        assert!(
            num(&a["minX"], &line) >= l - 0.5 && num(&a["maxX"], &line) <= w - rp + 0.5,
            "{name} area text outside the padding: {line}"
        );
        // Alignment: first line starts on the start-side padded edge, and the
        // widest wrapped line reaches it too (a centred area fails both).
        let (edge, p0) = if rtl {
            (w - rp, num(&a["maxX"], &line))
        } else {
            (l, num(&a["minX"], &line))
        };
        assert_eq!(
            num(&a["align"], &line),
            if rtl { ALIGN_RIGHT } else { ALIGN_LEFT },
            "{name} area wrong alignment: {line}"
        );
        assert!(
            (num(&a["p0x"], &line) - edge).abs() < 2.0,
            "{name} area first line not on the {edge} padded edge: {line}"
        );
        assert!(
            (p0 - edge).abs() < 2.0,
            "{name} area text never reaches the start-side padded edge: {line}"
        );
        let h = num(&a["h"], &line);
        assert!(h >= hit, "{name} area under Theme.hitMin high: {line}");
        assert!(
            h >= num(&a["contentH"], &line) + num(&a["top"], &line) + num(&a["bottom"], &line)
                - 0.5,
            "{name} area text taller than the box: {line}"
        );
        assert!(
            (num(&a["bodyW"], &line) - w).abs() < 0.5 && num(&a["bodyX"], &line).abs() < 0.5,
            "{name} area frame does not cover it: {line}"
        );
    }
}
