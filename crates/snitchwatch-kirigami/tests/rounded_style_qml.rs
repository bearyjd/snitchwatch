//! The rounded, iOS-style pass (PR A, pilot): every clickable thing on the two
//! pilot pages (About, the silent-mode choice) and on one of each restyled
//! wrapper is at least 44 px in both directions and not narrower than its own
//! content wants (so no label is cut), draws its corners and its focus ring
//! from `Theme`, shows the ring for keyboard focus only, and adds no Tab stop
//! of its own (the visuals are role-less and unfocusable). Radii and colours
//! must not be literals in the styled files (source guard below).
//!
//! The Tab itineraries themselves are asserted by the existing keyboard tests,
//! which this change leaves untouched. Run headless with
//! `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`; the Plasma
//! Breeze look is not what is measured here.
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
use common::{capture_stderr, init_headless_qt_env};

const MARKER: &str = "ROUNDED_STYLE_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000; height: 900
    property var about: null
    property var silent: null
    property var harness: null
    property int ticks: 0
    property bool focusDone: false
    property var focusResult: ({})
    readonly property var interactive: [Accessible.Button, Accessible.CheckBox,
        Accessible.RadioButton, Accessible.ComboBox, Accessible.EditableText,
        Accessible.Slider, Accessible.SpinBox, Accessible.MenuItem,
        Accessible.ButtonMenu, Accessible.ButtonDropDown, Accessible.Link]
    readonly property var radii: [Theme.radiusSmall, Theme.radiusMedium, Theme.radiusLarge, Theme.radiusPill]

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
    function inside(item, name) {
        for (let p = item; p; p = p.parent) if (p.objectName === name) return true;
        return false;
    }
    function controlsOf(root) {
        // `denyWarning` is a Kirigami.InlineMessage: Kirigami draws it and
        // reports a Button role for its text; it has no control of ours in it.
        return walk(root, [], new Set()).filter(i => drawn(i) && !probe.inside(i, "denyWarning")
            && probe.interactive.indexOf(i.Accessible.role) >= 0);
    }
    function surfacesOf(root) {
        return walk(root, [], new Set()).filter(i => i.focusRing !== undefined && i.cornerRadius !== undefined);
    }
    function describe(c) {
        return { name: c.Accessible.name || c.objectName || c.text || "?", w: c.width, h: c.height,
                 iw: c.implicitWidth, ih: c.implicitHeight };
    }
    function make(url, props, x, y) {
        const c = Qt.createComponent(url);
        if (c.status !== Component.Ready) throw new Error(c.errorString());
        return c.createObject(contentItem, Object.assign({ x: x, y: y }, props));
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
    // Composite fill (with its alpha) over the window, then the hover or
    // pressed wash over that, as drawn: wash is 0 (resting), hoverOpacity or
    // pressedOpacity.
    function fillWith(s, washOpacity) {
        const base = s.body.visible ? over(s.body.color, s.control.palette.window) : s.control.palette.window;
        const w = s.wash.color;
        return over(Qt.rgba(w.r, w.g, w.b, washOpacity), base);
    }
    // Contrast of the ring AS DRAWN (its own alpha included) against each state.
    function ringContrast(s) {
        const out = [];
        for (const wash of [0, Theme.hoverOpacity, Theme.pressedOpacity]) {
            const fill = fillWith(s, wash);
            out.push(ratio(over(s.focusRing.border.color, fill), fill));
        }
        return out;
    }
    function focusChecks(all) {
        const out = [];
        for (const c of all) {
            // A disabled control (no settings controller here) cannot take focus.
            if (!c.enabled) continue;
            c.forceActiveFocus(Qt.TabFocusReason);
            const s = surfacesOf(c).filter(x => x.control === c);
            const on = s.length > 0 && s[0].focusRing.visible && c.visualFocus;
            const ringWidth = s.length > 0 ? s[0].focusRing.border.width : -1;
            const contrast = s.length > 0 ? ringContrast(s[0]) : [-1];
            const ringAlpha = s.length > 0 ? s[0].focusRing.border.color.a * s[0].focusRing.opacity : -1;
            c.focus = false;
            c.forceActiveFocus(Qt.MouseFocusReason);
            const mouseOff = s.length > 0 && !s[0].focusRing.visible;
            out.push({ a2: c.activeFocus, vf2: c.visualFocus, r2: c.focusReason, ring2: s.length > 0 && s[0].focusRing.visible, name: describe(c).name, surface: s.length, on: on, contrast: contrast, ringAlpha: ringAlpha, mouseOff: mouseOff, ringWidth: ringWidth });
        }
        return out;
    }
    function report(state) {
        const pages = [about, silent, harness];
        const all = [];
        for (const p of pages) for (const c of controlsOf(p)) all.push(c);
        const surfaces = [];
        for (const p of pages) for (const s of surfacesOf(p)) surfaces.push(s);
        console.warn("ROUNDED_STYLE_RESULT " + JSON.stringify({
            state: state, ticks: ticks,
            hitMin: Theme.hitMin,
            controls: all.map(describe),
            radii: surfaces.map(s => ({ body: s.body.radius, ring: s.focusRing.radius,
                                         ok: probe.radii.indexOf(s.body.radius) >= 0 && s.body.radius === s.focusRing.radius })),
            focus: probe.focusResult,
            tabStops: pages.map(p => tabStops(p)),
            controlCounts: pages.map(p => controlsOf(p).length)
        }));
        Qt.quit();
    }
    Component.onCompleted: {
        about = make("qrc:/qt/qml/com/snitchwatch/shell/qml/AboutPage.qml",
            { width: 500, height: 400, flatpakInfoPath: "/nonexistent" }, 0, 0);
        silent = make("qrc:/qt/qml/com/snitchwatch/shell/qml/SilentModeChoice.qml",
            { width: 500, selected: "deny" }, 500, 0);
        harness = harnessComp.createObject(contentItem, { x: 0, y: 420, width: 900 });
    }
    Component {
        id: harnessComp
        RowLayout {
            spacing: 12
            GuardedSwitch { objectName: "hSwitch"; Accessible.name: "Harness switch"; text: "A switch" }
            GuardedSwitch { objectName: "hSwitchBare"; Accessible.name: "Bare switch" }
            GuardedToolButton { objectName: "hTool"; Accessible.name: "Harness tool"; icon.name: "edit-copy" }
            GuardedToolButton { objectName: "hToolText"; Accessible.name: "Tool text"; text: "Tool" }
            GuardedButton { objectName: "hGuarded"; text: "Guarded"; Accessible.name: "Guarded" }
            GuardedButton { objectName: "hGuardedPrimary"; text: "Primary"; highlighted: true; Accessible.name: "Primary" }
            GuardedButton { objectName: "hDark"; text: "Dark"; Accessible.name: "Dark"
                palette.window: "#202326"; palette.windowText: "#eff0f1"; palette.buttonText: "#eff0f1"
                palette.highlight: "#3daee9"; palette.highlightedText: "#ffffff" }
            GuardedButton { objectName: "hDarkPrimary"; text: "Dark primary"; highlighted: true; Accessible.name: "Dark primary"
                palette.window: "#202326"; palette.windowText: "#eff0f1"; palette.buttonText: "#eff0f1"
                palette.highlight: "#3daee9"; palette.highlightedText: "#ffffff" }
            RoundedButton { objectName: "hRounded"; text: "Plain rounded"; Accessible.name: "Plain rounded" }
            GuardedRadioButton { objectName: "hRadio"; text: "Radio"; Accessible.name: "Radio" }
        }
    }
    Timer {
        interval: 100; running: true; repeat: true
        onTriggered: {
            ticks += 1;
            try {
                if (!about || !silent || !harness) return;
                const ready = controlsOf(about).length >= 1 && controlsOf(silent).length >= 5
                    && controlsOf(harness).length >= 10;
                if (!ready) return;
                if (!focusDone) {
                    focusDone = true;
                    probe.requestActivate();
                    const all = [];
                    for (const p of [about, silent, harness]) for (const c of controlsOf(p)) all.push(c);
                    probe.focusResult = focusChecks(all);
                    return;
                }
                report("ready");
            } catch (e) {
                console.warn("ROUNDED_STYLE_ERROR " + String(e));
                Qt.quit();
            }
        }
    }
    // Independent of the polling above, so nothing can leave the event loop running.
    Timer {
        interval: 45000; running: true
        onTriggered: report("timeout")
    }
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
                &QUrl::from("qrc:/inline_rounded_style_probe.qml"),
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
    assert!(!captured.contains("Error:"), "QML error: {captured}");
    assert!(!captured.contains("ROUNDED_STYLE_ERROR"), "{captured}");
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    serde_json::from_str(&line).expect("probe JSON")
}

#[test]
fn the_pilot_controls_are_big_rounded_and_show_a_focus_ring() {
    let v = run_probe();
    let line = v.to_string();
    assert_eq!(v["state"], "ready", "not ready in time: {line}");
    assert!(
        v["hitMin"].as_f64().unwrap() >= 44.0,
        "Theme.hitMin: {line}"
    );
    let counts: Vec<u64> = v["controlCounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_u64().unwrap())
        .collect();
    // About: copy. Silent mode: 3 radios, review, continue. Harness: 10.
    assert_eq!(counts, vec![1, 5, 10], "controls found per page: {line}");

    for c in v["controls"].as_array().unwrap() {
        let (w, h) = (c["w"].as_f64().unwrap(), c["h"].as_f64().unwrap());
        assert!(w >= 44.0 && h >= 44.0, "hit box under 44 px: {c} in {line}");
        assert!(
            w + 0.5 >= c["iw"].as_f64().unwrap(),
            "narrower than its content wants (truncates): {c}"
        );
    }

    let radii = v["radii"].as_array().unwrap();
    assert!(radii.len() >= 14, "surfaces found: {line}");
    for r in radii {
        assert_eq!(r["ok"], true, "radius not from Theme: {r}");
    }

    let focus = v["focus"].as_array().unwrap();
    // Review and Continue are disabled here (no settings controller).
    assert_eq!(focus.len(), 14, "focus checks: {line}");
    for f in focus {
        assert_eq!(f["surface"], 1, "no rounded surface behind {f}");
        assert_eq!(f["on"], true, "no focus ring on keyboard focus: {f}");
        assert_eq!(f["mouseOff"], true, "focus ring shown for mouse focus: {f}");
        assert!(
            f["ringWidth"].as_f64().unwrap() >= 2.0,
            "ring too thin: {f}"
        );
        // The ring must stand out from the fill it sits on, in a light or a
        // dark palette, on a primary (highlight-filled) button or not.
        assert!(
            f["contrast"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c.as_f64().unwrap() >= 2.0),
            "focus ring does not contrast with its resting, hover and pressed fill: {f}"
        );
        assert_eq!(f["ringAlpha"], 1.0, "focus ring is not opaque: {f}");
    }

    // The ring, wash and fill are role-less and unfocusable: no extra Tab stop
    // beyond the controls themselves.
    let stops: Vec<u64> = v["tabStops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_u64().unwrap())
        .collect();
    assert_eq!(stops, counts, "extra Tab stops: {line}");
}

fn qml_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("qml")
        .join(format!("{name}.qml"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The 148 SVG / CSS named colours (lower case).
const NAMED_COLOURS: [&str; 148] = [
    "aliceblue",
    "antiquewhite",
    "aqua",
    "aquamarine",
    "azure",
    "beige",
    "bisque",
    "black",
    "blanchedalmond",
    "blue",
    "blueviolet",
    "brown",
    "burlywood",
    "cadetblue",
    "chartreuse",
    "chocolate",
    "coral",
    "cornflowerblue",
    "cornsilk",
    "crimson",
    "cyan",
    "darkblue",
    "darkcyan",
    "darkgoldenrod",
    "darkgray",
    "darkgreen",
    "darkgrey",
    "darkkhaki",
    "darkmagenta",
    "darkolivegreen",
    "darkorange",
    "darkorchid",
    "darkred",
    "darksalmon",
    "darkseagreen",
    "darkslateblue",
    "darkslategray",
    "darkslategrey",
    "darkturquoise",
    "darkviolet",
    "deeppink",
    "deepskyblue",
    "dimgray",
    "dimgrey",
    "dodgerblue",
    "firebrick",
    "floralwhite",
    "forestgreen",
    "fuchsia",
    "gainsboro",
    "ghostwhite",
    "gold",
    "goldenrod",
    "gray",
    "grey",
    "green",
    "greenyellow",
    "honeydew",
    "hotpink",
    "indianred",
    "indigo",
    "ivory",
    "khaki",
    "lavender",
    "lavenderblush",
    "lawngreen",
    "lemonchiffon",
    "lightblue",
    "lightcoral",
    "lightcyan",
    "lightgoldenrodyellow",
    "lightgray",
    "lightgreen",
    "lightgrey",
    "lightpink",
    "lightsalmon",
    "lightseagreen",
    "lightskyblue",
    "lightslategray",
    "lightslategrey",
    "lightsteelblue",
    "lightyellow",
    "lime",
    "limegreen",
    "linen",
    "magenta",
    "maroon",
    "mediumaquamarine",
    "mediumblue",
    "mediumorchid",
    "mediumpurple",
    "mediumseagreen",
    "mediumslateblue",
    "mediumspringgreen",
    "mediumturquoise",
    "mediumvioletred",
    "midnightblue",
    "mintcream",
    "mistyrose",
    "moccasin",
    "navajowhite",
    "navy",
    "oldlace",
    "olive",
    "olivedrab",
    "orange",
    "orangered",
    "orchid",
    "palegoldenrod",
    "palegreen",
    "paleturquoise",
    "palevioletred",
    "papayawhip",
    "peachpuff",
    "peru",
    "pink",
    "plum",
    "powderblue",
    "purple",
    "rebeccapurple",
    "red",
    "rosybrown",
    "royalblue",
    "saddlebrown",
    "salmon",
    "sandybrown",
    "seagreen",
    "seashell",
    "sienna",
    "silver",
    "skyblue",
    "slateblue",
    "slategray",
    "slategrey",
    "snow",
    "springgreen",
    "steelblue",
    "tan",
    "teal",
    "thistle",
    "tomato",
    "turquoise",
    "violet",
    "wheat",
    "white",
    "whitesmoke",
    "yellow",
    "yellowgreen",
];

/// `src` without `//` and `/* */` comments, string literals kept intact.
fn strip_comments(src: &str) -> String {
    let cs: Vec<char> = src.chars().collect();
    let (mut out, mut i, mut quote) = (String::new(), 0, None);
    while i < cs.len() {
        let c = cs[i];
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' && i + 1 < cs.len() {
                out.push(cs[i + 1]);
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else if matches!(c, '"' | '\'' | '`') {
            quote = Some(c);
            out.push(c);
        } else if c == '/' && cs.get(i + 1) == Some(&'/') {
            while i < cs.len() && cs[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == '/' && cs.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// The contents of every quoted string literal (single, double, backtick).
fn string_literals(code: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote) = (Vec::new(), String::new(), None);
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match quote {
            None if matches!(c, '"' | '\'' | '`') => {
                quote = Some(c);
                cur.clear();
            }
            Some(q) if c == q => {
                out.push(std::mem::take(&mut cur));
                quote = None;
            }
            Some(_) => {
                if c == '\\' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else {
                    cur.push(c);
                }
            }
            None => {}
        }
    }
    out
}

fn is_colour_text(lit: &str) -> bool {
    let t = lit.trim().to_ascii_lowercase();
    let hex = t.strip_prefix('#').is_some_and(|h| {
        !h.is_empty()
            && h.chars().all(|c| c.is_ascii_hexdigit())
            && matches!(h.len(), 3 | 4 | 6 | 8)
    });
    hex || ["rgb(", "rgba(", "hsl(", "hsla(", "hsv(", "hsva("]
        .iter()
        .any(|f| t.contains(f))
        || NAMED_COLOURS.contains(&t.as_str())
}

/// Colour literals anywhere in a styled QML file. A lint against ACCIDENTAL
/// literals, not a proof: a colour could still arrive from a non-literal
/// source (a number, a computed string, another file). The rule is file-wide,
/// so how a literal reaches a `color` (property, function, array, whitespace
/// before the colon) does not matter. Forbidden, outside comments: a quoted
/// string that is a named colour, `#hex` or `rgb(`/`hsl(` text; any
/// `Qt.rgba(`, `Qt.color(`, `Qt.hsla(`, `Qt.hsva(`; and `Qt.tint(`,
/// `Qt.lighter(`, `Qt.darker(` with a quoted literal argument. "transparent"
/// and every other string are allowed.
fn colour_violations(src: &str) -> Vec<String> {
    let code = strip_comments(src);
    let mut out: Vec<String> = string_literals(&code)
        .into_iter()
        .filter(|l| is_colour_text(l))
        .map(|l| format!("literal colour {l:?}"))
        .collect();
    for f in ["Qt.rgba(", "Qt.color(", "Qt.hsla(", "Qt.hsva("] {
        if code.contains(f) {
            out.push(format!("call {f}"));
        }
    }
    for f in ["Qt.tint(", "Qt.lighter(", "Qt.darker("] {
        for (at, _) in code.match_indices(f) {
            let args = code[at + f.len()..].split(')').next().unwrap_or("");
            if args.contains(['"', '\'', '`']) {
                out.push(format!("{f} with a literal"));
            }
        }
    }
    out
}

#[test]
fn the_colour_guard_rejects_literals_and_allows_the_palette() {
    for bad in [
        "color: \"red\"",
        "color : \"red\"",
        "border.color:\t'white'",
        "border.color: 'red'",
        "color: \"#ff0000\"",
        "color: \"#FFF\"",
        "color: \"rgb(1, 2, 3)\"",
        "color: `hsl(0, 100%%, 50%%)`",
        "color: Qt.rgba(1, 0, 0, 1)",
        "color: Qt.hsla(0, 1, 0.5, 1)",
        "color: control.down ? \"red\" : control.palette.base",
        "color: control.down\n    ? control.palette.base\n    : 'navy'",
        "color: Qt.alpha(\"red\", 0.5)",
        "color: Qt.alpha('Red', 0.5)",
        "color: Qt.color(\"blue\")",
        "color: Qt.lighter(\"green\", 1.2)",
        "color: Qt.tint(palette.base, \"orange\")",
        "property string fixed: \"red\"\ncolor: fixed",
        "function fixed() { return \"red\"; }\ncolor: fixed()",
        "property var shades: [\"red\", \"blue\"]\ncolor: shades[0]",
        "readonly property color c: 'tomato'",
        "/* note */ color: \"white\" // trailing",
    ] {
        assert!(!colour_violations(bad).is_empty(), "should reject: {bad}");
    }
    for ok in [
        "color: \"transparent\"",
        "border.color: 'transparent'",
        "color: control.palette.highlight",
        "color: Qt.alpha(control.palette.windowText, Theme.fillOpacity)",
        "color: Qt.lighter(control.palette.base, 1.2)",
        "// color: \"red\" in a comment only",
        "/* color: \"#ff0000\"\n   Qt.rgba(1,0,0,1) */\ncolor: control.palette.base",
        "text: \"red herring\"",
        "objectName: \"focusRing\"",
        "Accessible.name: \"Tan\\u00e9\" // not a colour",
    ] {
        assert!(
            colour_violations(ok).is_empty(),
            "should allow: {ok}: {:?}",
            colour_violations(ok)
        );
    }
}

/// The styled files hold no literal radii or colours, and no input area.
#[test]
fn the_styled_files_take_radii_and_colours_from_theme_and_the_palette() {
    let files = [
        ("RoundedSurface", qml_source("RoundedSurface")),
        ("RoundedButton", qml_source("RoundedButton")),
        ("GuardedButton", qml_source("GuardedButton")),
        ("GuardedToolButton", qml_source("GuardedToolButton")),
        ("GuardedSwitch", qml_source("GuardedSwitch")),
        ("GuardedRadioButton", qml_source("GuardedRadioButton")),
        ("RoundedFieldFrame", qml_source("RoundedFieldFrame")),
        ("RoundedField", qml_source("RoundedField")),
        ("RoundedTextArea", qml_source("RoundedTextArea")),
    ];
    for (name, src) in files {
        let code: String = src
            .as_str()
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            colour_violations(&src).is_empty(),
            "{name}: literal colour: {:?}",
            colour_violations(&src)
        );
        for (n, line) in code.lines().enumerate() {
            let t = line.trim();
            assert!(
                !(t.starts_with("radius:")
                    && t["radius:".len()..]
                        .trim()
                        .starts_with(|c: char| c.is_ascii_digit())),
                "{name}:{n}: literal radius: {t}"
            );
            assert!(
                !t.starts_with("MouseArea") && !t.starts_with("TapHandler"),
                "{name}:{n}: the visuals must take no input of their own: {t}"
            );
        }
    }
}
