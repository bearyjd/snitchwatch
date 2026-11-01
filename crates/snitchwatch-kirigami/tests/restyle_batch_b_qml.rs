//! Rounded-style batch B (plan `2026-10-30-rounded-ios-style.md`):
//! `SegmentedChoice`, `AlertWindow`, `PendingDecisionSheet`, `SizedOverlaySheet`.
//!
//! * `SegmentedChoice` is a joined capsule, restyled in place (its segments
//!   stay `Button`s, so the keyboard probes' names hold): one shared outline,
//!   hairline dividers, the chosen segment filled with the palette's
//!   `highlight`, outer corners rounded on the first and last segment
//!   (swapped in right-to-left), every segment at least `Theme.hitMin` high on
//!   a compact-style fixture, a focus ring for keyboard focus only that never
//!   equals the fill it sits on, no extra Tab stop, and Space / Return /
//!   arrows behaving as before.
//! * `AlertWindow` (the connection prompt): Allow and Deny are restyled in
//!   place, equal in size and look (neither is prominent), at least
//!   `Theme.hitMin` high, Deny drawn left of Allow, and the window still
//!   opens with the focus on its key catcher. Its keyboard behaviour is pinned
//!   unchanged by `alert_window_qml`, `alert_window_keyboard_qml` and
//!   `answer_surfaces_keyboard_qml`.
//! * Source guards: exactly which plain controls remain in each file, the
//!   in-place ones carry the clamp, the rounded background and their own
//!   label (`org.kde.desktop` draws a Button's text in the background it
//!   replaces, which Basic-style probes cannot see), palette-only
//!   colours, no literal radius, no input area.
//!
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
mod qml_guard_support;
use common::{capture_stderr, init_headless_qt_env};
use qml_guard_support::{blocks, code_lines};

const MARKER: &str = "RESTYLE_B_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000; height: 600
    property int phase: 0
    property var keyResult: null
    property var choiceResult: []
    property var focusResult: []

    TestCase { id: tc; name: "restyleB"; when: false }

    QtObject {
        id: feedStub
        function submitVerdict(rowId, choice, scope, duration, bindable) { return true; }
        function appBoundRulesFor(rowId) { return true; }
    }
    ConnectionsModel { id: connModel }
    AlertWindow {
        id: alert
        model: connModel
        bridgeFeed: feedStub
        armDelayMs: 100
    }

    readonly property var three: [{ label: "One", token: "a" }, { label: "Two", token: "b" },
                                  { label: "Three", token: "c" }]

    // Compact style: 8 px font, so only the component's own clamp gives 44 px.
    Column {
        id: stage
        x: 10; y: 10
        spacing: 10
        Controls.Pane {
            id: ltrPane
            padding: 0
            font.pixelSize: 8
            width: 420
            SegmentedChoice { id: ltr; width: 420; groupName: "LTR"; model: probe.three; currentToken: "b" }
        }
        Controls.Pane {
            padding: 0
            font.pixelSize: 8
            width: 420
            LayoutMirroring.enabled: true
            LayoutMirroring.childrenInherit: true
            SegmentedChoice { id: rtl; width: 420; groupName: "RTL"; model: probe.three; currentToken: "a" }
        }
        Controls.Pane {
            padding: 0
            font.pixelSize: 8
            width: 420
            SegmentedChoice { id: single; width: 420; groupName: "Single"; model: [{ label: "Once", token: "x" }] }
        }
        Controls.Pane {
            padding: 0
            width: 420
            palette.window: "#202326"; palette.windowText: "#eff0f1"; palette.buttonText: "#eff0f1"
            palette.button: "#31363b"; palette.highlight: "#3daee9"; palette.highlightedText: "#ffffff"
            SegmentedChoice { id: dark; width: 420; groupName: "Dark"; model: probe.three; currentToken: "c" }
        }
    }

    // The segment buttons of `s`, in model order.
    function repeated(s) {
        return walk(s, []).filter(it => s.indexOfItem(it) >= 0)
            .sort((a, b) => s.indexOfItem(a) - s.indexOfItem(b));
    }
    function walk(root, out) {
        if (!root || out.indexOf(root) >= 0) return out;
        out.push(root);
        for (const k of (root.children || [])) walk(k, out);
        if (root.background) walk(root.background, out);
        if (root.contentItem && root.contentItem !== root) walk(root.contentItem, out);
        return out;
    }
    function find(root, pred) { return walk(root, []).filter(pred)[0] || null; }
    function typeOf(o) { return String(o).split("(")[0]; }
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
    function effectiveOpacity(item, stopAt) {
        let o = 1;
        for (let i = item; i && i !== stopAt.parent; i = i.parent) o *= i.opacity;
        return o;
    }
    function corners(r) {
        return r ? [r.topLeftRadius, r.topRightRadius, r.bottomRightRadius, r.bottomLeftRadius] : null;
    }
    // Ring contrast against what it sits on: track, then (chosen) fill, then the wash.
    function ringContrast(s, b) {
        const bg = b.background;
        const track = s.track;
        let base = over(track.color, b.palette.window);
        if (bg.body.visible) base = over(bg.body.color, base);
        const out = [];
        for (const w of [0, Theme.hoverOpacity, Theme.pressedOpacity]) {
            const wc = bg.wash.color;
            const fill = over(Qt.rgba(wc.r, wc.g, wc.b, w), base);
            out.push(ratio(over(bg.focusRing.border.color, fill), fill));
        }
        return out;
    }
    function describeSeg(s, b, i) {
        const bg = b.background;
        const p = b.mapToItem(s, 0, 0);
        const label = b.contentItem;
        return { group: s.groupName, index: i, type: probe.typeOf(b), x: p.x, w: b.width, h: b.height,
                 iw: b.implicitWidth, font: b.font.pixelSize, mirrored: b.mirrored,
                 selected: b.highlighted, checkedA11y: b.Accessible.checked,
                 name: b.Accessible.name,
                 labelFits: label.implicitWidth <= b.availableWidth + 0.5,
                 fill: bg && bg.body ? { visible: bg.body.visible, corners: probe.corners(bg.body),
                                         isHighlight: Qt.colorEqual(bg.body.color, b.palette.highlight) } : null,
                 ring: bg && bg.focusRing ? { corners: probe.corners(bg.focusRing),
                                              width: bg.focusRing.border.width,
                                              isHighlight: Qt.colorEqual(bg.focusRing.border.color, b.palette.highlight) } : null,
                 wash: bg && bg.wash ? probe.corners(bg.wash) : null,
                 divider: bg && bg.divider ? { visible: bg.divider.visible, w: bg.divider.width,
                                               x: bg.divider.mapToItem(b, 0, 0).x } : null,
                 textColourOk: label.color !== undefined
                     && Qt.colorEqual(label.color, b.highlighted ? b.palette.highlightedText : b.palette.buttonText) };
    }
    function describeChoice(s) {
        const segs = probe.repeated(s);
        const t = s.track;
        return { group: s.groupName, h: s.height, count: segs.length,
                 track: t ? { radius: t.radius, h: t.height, w: t.width, border: t.border.width,
                              borderAlpha: t.border.color.a,
                              fillOpacity: t.color.a } : null,
                 segments: segs.map((b, i) => probe.describeSeg(s, b, i)),
                 tabStops: walk(s, []).filter(i => i.activeFocusOnTab === true && i.visible).length };
    }
    function focusChecks() {
        const out = [];
        for (const s of [ltr, rtl, single, dark]) {
            for (const b of probe.repeated(s)) {
                b.forceActiveFocus(Qt.TabFocusReason);
                const bg = b.background;
                const ring = bg ? bg.focusRing : null;
                const on = !!ring && ring.visible && b.visualFocus;
                const contrast = ring ? probe.ringContrast(s, b) : [-1];
                const opacity = ring ? probe.effectiveOpacity(ring, b) * ring.border.color.a : -1;
                const clash = !!ring && bg.body.visible && Qt.colorEqual(ring.border.color, bg.body.color);
                b.focus = false;
                b.forceActiveFocus(Qt.MouseFocusReason);
                out.push({ group: s.groupName, text: b.text, on: on, contrast: contrast, opacity: opacity,
                           clash: clash, mouseOff: !!ring && !ring.visible });
            }
        }
        return out;
    }
    // Space and Return pick the focused segment; arrows do nothing; Tab walks the segments.
    function keyChecks() {
        const segs = probe.repeated(ltr);
        ltr.currentToken = "b";
        segs[0].forceActiveFocus(Qt.TabFocusReason);
        tc.keyClick(Qt.Key_Space);
        const space = ltr.currentToken;
        segs[2].forceActiveFocus(Qt.TabFocusReason);
        tc.keyClick(Qt.Key_Return);
        const ret = ltr.currentToken;
        tc.keyClick(Qt.Key_Left);
        tc.keyClick(Qt.Key_Right);
        tc.keyClick(Qt.Key_Right);
        const arrows = ltr.currentToken + "@" + segs.indexOf(probe.activeFocusItem);
        segs[0].forceActiveFocus(Qt.TabFocusReason);
        tc.keyClick(Qt.Key_Tab);
        const tab1 = segs.indexOf(probe.activeFocusItem);
        tc.keyClick(Qt.Key_Tab);
        const tab2 = segs.indexOf(probe.activeFocusItem);
        return { space: space, ret: ret, arrows: arrows, tab: [tab1, tab2] };
    }
    function describeButton(b) {
        const p = b.mapToItem(null, 0, 0);
        const s = b.background;
        return { type: probe.typeOf(b), x: p.x, y: p.y, w: b.width, h: b.height, iw: b.implicitWidth,
                 ih: b.implicitHeight, enabled: b.enabled,
                 label: b.contentItem && b.contentItem.text !== undefined ? String(b.contentItem.text) : "",
                 labelColourOk: !!b.contentItem && b.contentItem.color !== undefined
                     && Qt.colorEqual(b.contentItem.color, b.palette.buttonText),
                 inkIsVerdict: Qt.colorEqual(b.palette.buttonText, b === alert.allowButton ? Theme.allow : Theme.deny),
                 surface: s && s.focusRing !== undefined && s.cornerRadius !== undefined
                     ? { prominent: s.prominent, flat: s.flat, tinted: s.tinted, pill: s.pill,
                         ringIsFill: Qt.colorEqual(s.ringColor, s.body.color) } : null };
    }
    function alertReport() {
        return { shown: alert.shown, armed: alert.armed, fixedHeight: alert.fixedHeight,
                 height: alert.height, width: alert.width,
                 focusOnKeys: alert.activeFocusItem === alert.keyCatcher,
                 allow: describeButton(alert.allowButton), deny: describeButton(alert.denyButton),
                 scope: describeChoice(alert.scopeSelector), duration: describeChoice(alert.durationSelector) };
    }

    function finish(state) {
        console.warn("RESTYLE_B_RESULT " + JSON.stringify({
            state: state, hitMin: Theme.hitMin, focusRingWidth: Theme.focusRingWidth,
            choices: probe.choiceResult,
            focus: probe.focusResult, keys: probe.keyResult,
            alert: probe.alertSnapshot !== null ? probe.alertSnapshot : probe.alertReport() }));
        Qt.quit();
    }

    Timer {
        interval: 50; running: true; repeat: true
        onTriggered: {
            try {
                if (probe.phase === 0) {
                    const row = { id: "r1", process: "p", processPath: "/usr/bin/x", dstHost: "example.com",
                                  dstIp: "93.184.216.34", dstPort: 443, protocol: "tcp", direction: "outgoing",
                                  action: null, bytesSent: 0, bytesReceived: 0, startedAtMs: 0,
                                  answerDeadlineMs: Date.now() + 600000 };
                    connModel.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows", rows: [row] }));
                    alert.refresh();
                    probe.phase = 1;
                } else if (probe.phase === 1 && alert.shown && alert.armed) {
                    // The alert's own focus state, before this window takes the keyboard.
                    probe.phase = 2;
                    probe.alertSnapshot = probe.alertReport();
                } else if (probe.phase === 2) {
                    probe.requestActivate();
                    probe.phase = 3;
                } else if (probe.phase === 3) {
                    probe.choiceResult = [ltr, rtl, single, dark].map(probe.describeChoice);
                    probe.focusResult = probe.focusChecks();
                    probe.keyResult = probe.keyChecks();
                    probe.finish("ready");
                }
            } catch (e) {
                console.warn("RESTYLE_B_ERROR " + String(e) + " " + e.stack);
                Qt.quit();
            }
        }
    }
    property var alertSnapshot: null
    Timer { interval: 30000; running: true; onTriggered: probe.finish("timeout") }
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
                &QUrl::from("qrc:/inline_restyle_batch_b_probe.qml"),
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
    assert!(!captured.contains("RESTYLE_B_ERROR"), "{captured}");
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    serde_json::from_str(&line).expect("probe JSON")
}

fn f(v: &serde_json::Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

/// Outer corners: the visually leftmost segment rounds its left corners, the
/// rightmost its right ones; inner corners are square. One segment: all four.
fn assert_corners(seg: &serde_json::Value, key: &str, leftmost: bool, rightmost: bool) {
    let corners = if key == "wash" {
        &seg["wash"]
    } else {
        &seg[key]["corners"]
    };
    let c: Vec<f64> = corners
        .as_array()
        .unwrap_or_else(|| panic!("no {key} corners: {seg}"))
        .iter()
        .map(f)
        .collect();
    let half = f(&seg["h"]) / 2.0;
    let want = |round: bool| if round { half } else { 0.0 };
    // [topLeft, topRight, bottomRight, bottomLeft]
    let expect = [
        want(leftmost),
        want(rightmost),
        want(rightmost),
        want(leftmost),
    ];
    for (got, want) in c.iter().zip(expect) {
        assert!(
            (got - want).abs() < 0.5,
            "{key} corners {c:?}, want {expect:?}: {seg}"
        );
    }
}

#[test]
fn segmented_choice_is_a_joined_capsule_and_the_alert_buttons_are_big_and_equal() {
    let v = run_probe();
    let line = v.to_string();
    assert_eq!(v["state"], "ready", "not ready in time: {line}");
    let hit_min = f(&v["hitMin"]);
    assert!(hit_min >= 44.0, "Theme.hitMin: {line}");

    let choices = v["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 4, "{line}");
    for (choice, rtl) in choices.iter().zip([false, true, false, false]) {
        let segs = choice["segments"].as_array().unwrap();
        assert_eq!(f(&choice["count"]) as usize, segs.len(), "{choice}");
        assert!(!segs.is_empty(), "{choice}");
        // Every segment is a Tab stop and nothing else in the strip is.
        assert_eq!(
            f(&choice["tabStops"]) as usize,
            segs.len(),
            "extra Tab stop: {choice}"
        );
        // The shared outline: a hairline capsule as tall as the strip.
        let track = &choice["track"];
        assert!(track.is_object(), "no shared track: {choice}");
        assert!(
            f(&track["border"]) > 0.0 && f(&track["borderAlpha"]) > 0.0,
            "track has no outline: {choice}"
        );
        assert!(
            f(&track["fillOpacity"]) > 0.0,
            "track has no fill: {choice}"
        );
        assert!(
            (f(&track["radius"]) - f(&track["h"]) / 2.0).abs() < 0.5,
            "track is not a capsule: {choice}"
        );
        assert!(
            f(&track["h"]) >= hit_min,
            "track lower than hitMin: {choice}"
        );

        let mut by_x: Vec<&serde_json::Value> = segs.iter().collect();
        by_x.sort_by(|a, b| f(&a["x"]).partial_cmp(&f(&b["x"])).unwrap());
        // Right-to-left: the first choice sits on the right.
        let first_is_left = by_x[0]["index"] == 0;
        if segs.len() > 1 {
            assert_eq!(first_is_left, !rtl, "mirroring of the order: {choice}");
        }
        let n = by_x.len();
        for (pos, seg) in by_x.iter().enumerate() {
            assert_eq!(
                seg["type"].as_str().unwrap().split('_').next(),
                Some("Button"),
                "segment type name changed: {seg}"
            );
            assert_eq!(
                seg["mirrored"], rtl,
                "LayoutMirroring did not reach the segment: {seg}"
            );
            assert!(
                f(&seg["h"]) >= hit_min && f(&seg["w"]) >= hit_min,
                "hit box under hitMin: {seg}"
            );
            assert_eq!(seg["labelFits"], true, "label clipped: {seg}");
            assert_eq!(
                seg["selected"], seg["checkedA11y"],
                "Accessible.checked: {seg}"
            );
            assert_eq!(
                seg["textColourOk"], true,
                "label colour not from the palette role: {seg}"
            );
            let leftmost = pos == 0;
            let rightmost = pos == n - 1;
            assert_corners(seg, "fill", leftmost, rightmost);
            assert_corners(seg, "ring", leftmost, rightmost);
            assert_corners(seg, "wash", leftmost, rightmost);
            assert_eq!(
                seg["fill"]["visible"], seg["selected"],
                "only the chosen segment is filled: {seg}"
            );
            assert_eq!(
                seg["fill"]["isHighlight"], true,
                "chosen fill is not the palette highlight: {seg}"
            );
            assert_eq!(
                f(&seg["ring"]["width"]),
                f(&v["focusRingWidth"]),
                "ring width: {seg}"
            );
            if seg["selected"] == true {
                assert_eq!(
                    seg["ring"]["isHighlight"], false,
                    "ring equals the chosen fill: {seg}"
                );
            }
            // Hairline dividers between segments, none on the outer left edge,
            // and none next to the chosen one.
            let divider = &seg["divider"];
            assert!(divider.is_object(), "no divider: {seg}");
            let left_neighbour_selected = pos > 0 && by_x[pos - 1]["selected"] == true;
            let want = !leftmost && seg["selected"] != true && !left_neighbour_selected;
            assert_eq!(divider["visible"], want, "divider visibility: {seg}");
            assert!(
                f(&divider["w"]) > 0.0 && f(&divider["w"]) <= 1.0,
                "not a hairline: {seg}"
            );
            assert!(
                f(&divider["x"]).abs() < 0.5,
                "divider not on the left edge: {seg}"
            );
        }
        if segs.len() > 1 {
            let w0 = f(&segs[0]["w"]);
            assert!(
                segs.iter().all(|s| (f(&s["w"]) - w0).abs() < 1.0),
                "unequal segments: {choice}"
            );
        }
    }
    // The compact fixture really is compact (8 px), so the clamp is what gives 44.
    assert_eq!(f(&choices[0]["segments"][0]["font"]), 8.0, "{line}");

    let focus = v["focus"].as_array().unwrap();
    assert_eq!(focus.len(), 10, "focus checks: {line}");
    for c in focus {
        assert_eq!(c["on"], true, "no ring on keyboard focus: {c}");
        assert_eq!(c["mouseOff"], true, "ring shown for mouse focus: {c}");
        assert_eq!(c["clash"], false, "ring equals the fill under it: {c}");
        assert!(f(&c["opacity"]) > 0.5, "ring (nearly) transparent: {c}");
        assert!(
            c["contrast"]
                .as_array()
                .unwrap()
                .iter()
                .all(|x| f(x) >= 2.0),
            "ring does not contrast with what it sits on: {c}"
        );
    }

    let keys = &v["keys"];
    assert_eq!(
        keys["space"], "a",
        "Space did not pick the focused segment: {keys}"
    );
    assert_eq!(
        keys["ret"], "c",
        "Return did not pick the focused segment: {keys}"
    );
    assert_eq!(
        keys["arrows"], "c@2",
        "arrows changed the choice or the focus: {keys}"
    );
    assert_eq!(
        keys["tab"],
        serde_json::json!([1, 2]),
        "Tab order across segments: {keys}"
    );

    // The connection prompt, as it opened (before the probe took the keyboard).
    let a = &v["alert"];
    assert_eq!(a["shown"], true, "{a}");
    let (allow, deny) = (&a["allow"], &a["deny"]);
    for b in [allow, deny] {
        assert_eq!(
            b["type"].as_str().unwrap().split('_').next(),
            Some("Button"),
            "verdict button type name changed: {b}"
        );
        assert!(
            f(&b["h"]) >= hit_min && f(&b["w"]) >= hit_min,
            "verdict hit box: {b}"
        );
        assert!(
            f(&b["w"]) + 0.5 >= f(&b["iw"]),
            "verdict button truncates: {b}"
        );
        assert_eq!(b["labelColourOk"], true, "verdict label colour: {b}");
        assert_eq!(b["inkIsVerdict"], true, "verdict colour lost: {b}");
        let s = &b["surface"];
        assert!(
            s.is_object(),
            "verdict button not drawn by RoundedSurface: {b}"
        );
        assert_eq!(s["prominent"], false, "one verdict made prominent: {b}");
        assert_eq!(s["ringIsFill"], false, "ring equals fill: {b}");
    }
    assert_eq!(
        allow["surface"], deny["surface"],
        "Allow and Deny look different: {a}"
    );
    assert_eq!(allow["label"], "Allow", "{a}");
    assert_eq!(deny["label"], "Deny", "{a}");
    assert!(
        (f(&allow["w"]) - f(&deny["w"])).abs() < 0.5,
        "unequal widths: {a}"
    );
    assert!(
        (f(&allow["h"]) - f(&deny["h"])).abs() < 0.5,
        "unequal heights: {a}"
    );
    assert!(
        (f(&allow["y"]) - f(&deny["y"])).abs() < 0.5,
        "not on one row: {a}"
    );
    assert!(
        f(&deny["x"]) < f(&allow["x"]),
        "Deny no longer drawn left of Allow: {a}"
    );
    for key in ["scope", "duration"] {
        for seg in a[key]["segments"].as_array().unwrap() {
            assert!(f(&seg["h"]) >= hit_min, "alert segment under hitMin: {seg}");
            assert_eq!(seg["labelFits"], true, "alert segment label clipped: {seg}");
        }
    }
    assert_eq!(a["duration"]["count"], 4, "{a}");
    // The window is sized from its content and stays that size.
    assert_eq!(a["height"], a["fixedHeight"], "{a}");
}

#[test]
fn the_alert_opens_with_the_focus_on_its_key_catcher() {
    // Pinned by alert_window_keyboard_qml (Return = Allow from the start);
    // this guards the source half: the shown handler still resets the focus.
    let code = code_lines(include_str!("../qml/AlertWindow.qml"));
    for needle in [
        "if (alert.shown) { alert.resetFocus();",
        "keys.forceActiveFocus();",
        "Keys.onPressed: event => alert.verdictButtonPressed(event, \"allow\")",
        "Keys.onPressed: event => alert.verdictButtonPressed(event, \"deny\")",
    ] {
        assert!(
            code.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(needle),
            "AlertWindow lost `{needle}`"
        );
    }
}

fn qml_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("qml")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Count `needle` at the start of a (trimmed) code line or after a
/// `delegate:`, so `Controls.Button` does not match `GuardedButton`.
fn sites(code: &str, needle: &str) -> usize {
    code.lines()
        .map(|l| l.trim_start())
        .map(|l| {
            l.strip_prefix("delegate:")
                .map(str::trim_start)
                .unwrap_or(l)
        })
        .filter(|l| l.starts_with(needle))
        .count()
}

/// (file, plain controls left: (Controls.Button, Controls.Switch, RadioButton,
/// ToolButton, CheckBox, ComboBox), restyled in place (of the plain Buttons),
/// swapped: (RoundedButton, RoundedComboBox)).
/// Documented exceptions: every plain Button left is restyled in place (the
/// segments and the alert's Allow/Deny keep the type name the keyboard probes
/// read); `SizedOverlaySheet` holds no control at all (its header and tooltip
/// are Kirigami / text); `DecideLaterButton` in the sheet is its own file and
/// not in this batch.
const BATCH: &[(&str, [usize; 6], usize, [usize; 2])] = &[
    ("SegmentedChoice.qml", [1, 0, 0, 0, 0, 0], 1, [0, 0]),
    ("AlertWindow.qml", [2, 0, 0, 0, 0, 0], 2, [0, 0]),
    ("PendingDecisionSheet.qml", [0; 6], 0, [2, 2]),
    ("SizedOverlaySheet.qml", [0; 6], 0, [0, 0]),
];

#[test]
fn the_batch_files_keep_only_restyled_controls() {
    let kinds = [
        "Controls.Button {",
        "Controls.Switch {",
        "Controls.RadioButton {",
        "Controls.ToolButton {",
        "Controls.CheckBox {",
        "Controls.ComboBox {",
    ];
    for (name, plain, in_place, swapped) in BATCH {
        let code = code_lines(&qml_source(name));
        for (kind, want) in kinds.iter().zip(plain) {
            assert_eq!(sites(&code, kind), *want, "{name}: plain `{kind}` sites");
        }
        assert_eq!(
            sites(&code, "RoundedButton {"),
            swapped[0],
            "{name}: RoundedButton sites"
        );
        assert_eq!(
            sites(&code, "RoundedComboBox {"),
            swapped[1],
            "{name}: RoundedComboBox sites"
        );
        // Every plain Button left carries the clamp and a rounded background.
        let buttons = blocks(&code, "Controls.Button {");
        assert_eq!(buttons.len(), *in_place, "{name}: in-place Buttons");
        for b in buttons {
            let flat = b.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                flat.contains("implicitHeight: Math.max(Theme.hitMin,"),
                "{name}: a plain Button without the hitMin clamp:\n{b}"
            );
            assert!(
                flat.contains("background: RoundedSurface {")
                    || flat.contains("objectName: \"segmentSurface\""),
                "{name}: a plain Button without the rounded background:\n{b}"
            );
            // org.kde.desktop draws a Button's label inside its background:
            // replacing the background without a content item blanks it.
            assert!(
                flat.contains("contentItem: Impl.IconLabel {") && flat.contains(".text"),
                "{name}: a restyled Button without its own label (blank on Breeze):\n{b}"
            );
        }
    }
}

/// Literal colours (hex, colour constructors, named colours), outside comments.
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
fn the_batch_files_take_colours_from_the_palette_and_radii_from_theme() {
    for (name, _, _, _) in BATCH {
        let code = code_lines(&qml_source(name));
        assert!(
            colour_violations(&code).is_empty(),
            "{name}: {:?}",
            colour_violations(&code)
        );
        for (n, line) in code.lines().enumerate() {
            let t = line.trim();
            let radius = t.split(':').next().unwrap_or("").trim().ends_with("adius");
            assert!(
                !(radius
                    && t.split_once(':')
                        .unwrap()
                        .1
                        .trim()
                        .starts_with(|c: char| c.is_ascii_digit())
                    && !t.split_once(':').unwrap().1.trim().starts_with("0")),
                "{name}:{n}: literal radius: {t}"
            );
        }
    }
    // The segment visuals take no input of their own.
    let seg = code_lines(&qml_source("SegmentedChoice.qml"));
    for t in seg.lines().map(str::trim) {
        assert!(
            !t.starts_with("MouseArea")
                && !t.starts_with("TapHandler")
                && !t.starts_with("HoverHandler"),
            "SegmentedChoice: input of its own: {t}"
        );
    }
    assert!(colour_violations("color: \"#ff0000\"").len() == 1);
    assert!(colour_violations("color: Qt.alpha(seg.palette.windowText, 0.1)").is_empty());
}
