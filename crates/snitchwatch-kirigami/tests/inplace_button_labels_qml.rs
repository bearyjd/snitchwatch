//! A Button / ToolButton / ComboBox restyled with its own `background:` must
//! also draw its own label (`contentItem:`).
//!
//! KDE's native Qt Quick Controls style (`org.kde.desktop`, what the app gets
//! in a real Plasma session) draws a Button's, ToolButton's, DelayButton's
//! and a non-editable ComboBox's text INSIDE its `background` (a
//! `StyleItem`); its `contentItem` is null (Button / ToolButton) or hidden
//! (ComboBox). Replacing only the background therefore leaves a blank
//! button there. The Basic style used headlessly supplies its own
//! `contentItem`, so ordinary probes cannot see it.
//!
//! * A source guard: every such block in `qml/` that sets `background:` at
//!   its own top level also sets `contentItem:` at its own top level.
//!   CheckBox / RadioButton / Switch are not checked: org.kde.desktop gives
//!   those a `Label` contentItem of their own, outside the background.
//! * A runtime probe: the fixed buttons' own label item (by objectName) is
//!   visible, carries the button's text and sits inside the button. It runs
//!   under whatever `QT_QUICK_CONTROLS_STYLE` is set (Basic by default); with
//!   `QT_QUICK_CONTROLS_STYLE=org.kde.desktop` (and a `QML_IMPORT_PATH` that
//!   provides it) it also proves that style really loaded, by checking that a
//!   plain Button's `contentItem` is null there.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::about_model as _;
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

/// Controls whose native (org.kde.desktop) label lives in the background.
const LABEL_IN_BACKGROUND: [&str; 4] = [
    "Controls.Button",
    "Controls.ToolButton",
    "Controls.DelayButton",
    "Controls.ComboBox",
];

/// Source with comments removed and string contents blanked (quotes kept),
/// so braces and keywords inside either never count.
fn scrub(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                if chars[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            out.push(c);
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                } else if chars[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            out.push(c);
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The `name: ` property bindings written directly inside the block whose
/// opening `{` is at byte `open` of `code` (not inside nested objects).
fn top_level_props(code: &str, open: usize) -> Vec<String> {
    let mut props = Vec::new();
    let mut depth = 0usize;
    let mut line_start = true;
    let mut line = String::new();
    for c in code[open..].chars() {
        match c {
            '{' => {
                depth += 1;
                line_start = depth == 1;
                line.clear();
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                line.clear();
            }
            ';' | '\n' if depth == 1 => {
                line_start = true;
                line.clear();
            }
            _ if depth == 1 && line_start => {
                line.push(c);
                let t = line.trim_start();
                if let Some(name) = t.strip_suffix(':').map(str::trim_end) {
                    if !name.is_empty()
                        && name
                            .chars()
                            .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.')
                    {
                        props.push(name.to_string());
                        line_start = false;
                    }
                } else if t.contains([' ', '(', '{', '[']) && !t.ends_with(char::is_whitespace) {
                    line_start = false;
                }
            }
            _ => {}
        }
    }
    props
}

/// `(type, line)` of every label-in-background control in `src` that sets
/// `background:` but not `contentItem:` at its own top level.
fn blank_label_blocks(src: &str) -> Vec<(String, usize)> {
    let code = scrub(src);
    let mut out = Vec::new();
    for ty in LABEL_IN_BACKGROUND {
        for (at, _) in code.match_indices(ty) {
            let before_ok = code[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'));
            let rest = &code[at + ty.len()..];
            let after = rest.trim_start();
            if !before_ok || !after.starts_with('{') {
                continue;
            }
            let open = at + ty.len() + (rest.len() - after.len());
            let props = top_level_props(&code, open);
            if props.iter().any(|p| p == "background") && !props.iter().any(|p| p == "contentItem")
            {
                out.push((ty.to_string(), code[..at].matches('\n').count() + 1));
            }
        }
    }
    out
}

#[test]
fn the_label_guard_rejects_a_bare_background_and_accepts_an_own_label() {
    let bad = [
        // Root object (GuardedToolButton's shape).
        "import QtQuick.Controls as Controls\nControls.ToolButton {\n    background: Rectangle {}\n}\n",
        // Nested, with a nested contentItem that must not count.
        "Item {\n    Controls.Button {\n        id: b\n        background: RoundedSurface { control: b }\n        \
         Item { contentItem: null }\n        text: \"x\"\n    }\n}\n",
        // Popup's contentItem is not the combo's.
        "Controls.ComboBox {\n    background: Rectangle {}\n    popup: Controls.Popup {\n        \
         contentItem: ListView {}\n    }\n}\n",
        "Item { Controls.Button { background: Rectangle {}; text: \"x\" } }",
        // Whitespace before the colon must not hide the property name.
        "Controls.ToolButton { background : Rectangle {} }",
        "Controls.ToolButton {\n    background\t: Rectangle {}\n}\n",
        "Controls.Button {\n    background :\n        Rectangle {}\n}\n",
    ];
    for src in bad {
        assert_eq!(blank_label_blocks(src).len(), 1, "should reject:\n{src}");
    }
    let good = [
        "Controls.Button {\n    background: Rectangle {}\n    contentItem: Impl.IconLabel { text: \"x\" }\n}\n",
        "Controls.ToolButton { background: Rectangle {}; contentItem: Text {} }",
        // No background replaced: the style draws its own label.
        "Controls.Button {\n    text: \"Native\"\n    // background: Rectangle {}\n}\n",
        // Wrappers inherit their label; braces/keywords in strings never count.
        "RoundedButton { background: Rectangle {} }\nText { text: \"Controls.Button { background: x }\" }\n",
        // Check box family: org.kde.desktop's own Label contentItem survives.
        "Controls.Switch { background: Rectangle {} }",
        // Spaced / tabbed colons on a valid control.
        "Controls.Button {\n    background : Rectangle {}\n    contentItem : Text {}\n}\n",
        "Controls.Button {\n    background\t: Rectangle {}\n    contentItem\t:\n        Text {}\n}\n",
    ];
    for src in good {
        assert!(blank_label_blocks(src).is_empty(), "should accept:\n{src}");
    }
}

#[test]
fn no_restyled_button_in_qml_leaves_its_label_to_the_native_style() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qml");
    let mut violations = Vec::new();
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).expect("qml dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("qml") {
            continue;
        }
        scanned += 1;
        let src = std::fs::read_to_string(&path).expect("qml source");
        for (ty, line) in blank_label_blocks(&src) {
            violations.push(format!("{}:{line} {ty}", path.display()));
        }
    }
    assert!(
        scanned > 50,
        "scanned only {scanned} files in {}",
        dir.display()
    );
    assert!(
        violations.is_empty(),
        "a restyled control sets `background:` without its own `contentItem:`; \
         org.kde.desktop draws its label in the background, so it renders blank there:\n{}",
        violations.join("\n")
    );
}

const MARKER: &str = "INPLACE_LABELS_RESULT";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000; height: 900
    property var barModel: null

    function walk(root, out) {
        if (!root || out.indexOf(root) >= 0) return out;
        out.push(root);
        for (const k of (root.children || [])) walk(k, out);
        if (root.contentItem && root.contentItem !== root) walk(root.contentItem, out);
        return out;
    }
    function typeOf(o) { return String(o).split("(")[0]; }
    function make(url, props) {
        const c = Qt.createComponent(url);
        if (c.status !== Component.Ready) throw new Error(c.errorString());
        return c.createObject(stage, props);
    }
    function report(b, labelName) {
        const l = b ? b.contentItem : null;
        return {
            name: b ? b.objectName : "", found: !!b, type: b ? typeOf(b) : "",
            text: b ? b.text : "", accessibleName: b ? b.Accessible.name : "",
            h: b ? b.height : -1,
            ownLabel: !!l && l.objectName === labelName,
            labelText: l && l.text !== undefined ? String(l.text) : "",
            labelVisible: !!l && l.visible && l.width > 0 && l.height > 0 && l.opacity > 0,
            labelInside: !!l && l.x >= -0.5 && l.y >= -0.5
                && l.x + l.width <= b.width + 0.5 && l.y + l.height <= b.height + 0.5,
            labelFits: !!l && l.implicitWidth <= l.width + 0.5,
            colourOk: !!l && l.color !== undefined && Qt.colorEqual(l.color, b.palette.buttonText)
        };
    }

    Item { id: stage; anchors.fill: parent }
    // A plain Button: null contentItem only under org.kde.desktop.
    Controls.Button { id: nativeRef; text: "Native"; x: 600; y: 600 }

    Timer {
        interval: 300; running: true
        onTriggered: {
            try {
                barModel = Qt.createQmlObject('import QtQuick; QtObject { property bool filterNeedsAttention: true;'
                    + ' property int backgroundCount: 3; property int backgroundPrograms: 2; property bool backgroundAvailable: true;'
                    + ' property int newProgramCount: 2; property string baselineNotice: "n"; property int matchingCount: 1;'
                    + ' property int totalCount: 4; function setNeedsAttention(v) {} function markNewProgramsSeen() {}'
                    + ' function dismissBaselineNotice() {} }', probe);
                const bar = make("qrc:/qt/qml/com/snitchwatch/shell/qml/BackgroundGroupBar.qml",
                    { model: barModel, width: 700, x: 10, y: 10 });
                const about = make("qrc:/qt/qml/com/snitchwatch/shell/qml/AboutPage.qml",
                    { x: 10, y: 300, width: 500, height: 500 });
                const tools = [
                    make("qrc:/qt/qml/com/snitchwatch/shell/qml/GuardedToolButton.qml",
                        { text: "Fit", objectName: "toolText", x: 600, y: 10 }),
                    make("qrc:/qt/qml/com/snitchwatch/shell/qml/GuardedToolButton.qml",
                        { text: "Places", objectName: "toolIcon", x: 600, y: 80,
                          "icon.name": "view-list-details" })
                ];
                const all = walk(bar, []).concat(walk(about, []));
                const buttons = [];
                for (const n of ["showAllButton", "backgroundToggle", "markSeenButton", "dismissBaseline",
                                 "aboutCopyButton"]) {
                    buttons.push(report(all.filter(i => i.objectName === n)[0], "inPlaceButtonLabel"));
                }
                for (const t of tools) buttons.push(report(t, "toolButtonLabel"));
                console.warn("INPLACE_LABELS_RESULT " + JSON.stringify({
                    hitMin: Theme.hitMin, nativeNullLabel: nativeRef.contentItem === null, buttons: buttons }));
            } catch (e) {
                console.warn("INPLACE_LABELS_ERROR " + String(e));
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
                &QUrl::from("qrc:/inline_inplace_labels_probe.qml"),
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
    assert!(!captured.contains("INPLACE_LABELS_ERROR"), "{captured}");
    assert!(!captured.contains("Error:"), "QML error: {captured}");
    let line = captured
        .lines()
        .find_map(|l| l.split_once(MARKER).map(|(_, r)| r.trim().to_string()))
        .unwrap_or_else(|| panic!("probe never reported: {captured}"));
    serde_json::from_str(&line).expect("probe JSON")
}

#[test]
fn restyled_buttons_draw_their_own_visible_label() {
    let v = run_probe();
    let hit = v["hitMin"].as_f64().unwrap();
    let kde = std::env::var("QT_QUICK_CONTROLS_STYLE").as_deref() == Ok("org.kde.desktop");
    // Proves which style really ran: org.kde.desktop leaves a plain Button
    // without a contentItem (its label is in the background), Basic does not.
    assert_eq!(
        v["nativeNullLabel"], kde,
        "style not the one requested (kde={kde}): {v}"
    );

    let buttons = v["buttons"].as_array().unwrap();
    assert_eq!(buttons.len(), 7, "{v}");
    for b in buttons {
        assert_eq!(b["found"], true, "{b}");
        assert_eq!(
            b["ownLabel"], true,
            "the native style's label is used (blank on Breeze): {b}"
        );
        assert!(!b["text"].as_str().unwrap().is_empty(), "{b}");
        assert_eq!(
            b["labelText"], b["text"],
            "label text differs from the button's: {b}"
        );
        assert_eq!(b["labelVisible"], true, "label not drawn: {b}");
        assert_eq!(b["labelInside"], true, "label outside the button: {b}");
        assert_eq!(b["labelFits"], true, "label clipped: {b}");
        assert_eq!(
            b["colourOk"], true,
            "label colour is not palette.buttonText: {b}"
        );
        assert!(b["h"].as_f64().unwrap() >= hit, "button under hitMin: {b}");
        let ty = b["type"].as_str().unwrap();
        if b["name"].as_str().unwrap().starts_with("tool") {
            assert!(
                ty.starts_with("GuardedToolButton"),
                "type name changed: {b}"
            );
        } else {
            assert!(ty.starts_with("Button_QMLTYPE"), "type name changed: {b}");
        }
    }
    let copy = buttons
        .iter()
        .find(|b| b["name"] == "aboutCopyButton")
        .unwrap();
    assert_eq!(copy["accessibleName"], "Copy version info", "{copy}");
}
