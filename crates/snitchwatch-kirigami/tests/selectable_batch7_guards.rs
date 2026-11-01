//! Selectable text, batch 7 (Prompt and sheets; plan
//! `2026-10-30-selectable-text.md`). Source guards: the class-A texts of the
//! decision and rule sheets, the inspector panel and the connection
//! explanation are `SelectableText`; the texts that stay a `Controls.Label` /
//! `Kirigami.Heading` are exactly the listed, justified ones.
//!
//! The alert window is the security surface and keeps every Label: each
//! `SelectableText` brings its own right-click `Controls.Menu` (a popup that
//! takes keyboard focus while open, so Esc would close it instead of
//! answering Deny and Return would go to its item) and a window-wide Ctrl+C
//! `Shortcut`. The alert's program and path are elided (wrapping them would
//! move the buttons under the pointer) and its countdown is re-written every
//! second. The same data is copyable from the inspector.
//!
//! None of these files has a clickable row (class C), so no `RowCopyMenu`, and
//! no converted text sits inside a click target (`CLICK_TARGETS`) or has one
//! inside it, and no block holding one has a sibling TapHandler / MouseArea /
//! DragHandler / PointHandler / HoverHandler (those target the shared parent).
//! `selectable_batch7_focus_qml.rs` checks the focus side at runtime.
//!
//! Mouse only: no converted text may add a Tab stop or override
//! `activeFocusOnPress` (false in `SelectableText.qml`). PlainText stays
//! explicit on every converted block (the data here is the connecting
//! program's, the remote host's and the firewall's). Layout unchanged: each
//! converted block's `wrapMode` is pinned per block in `WRAPS`.
mod qml_guard_support;
use qml_guard_support::{blocks, code_lines};

/// (file, source, SelectableText blocks expected, snippets that identify each
/// text still left as a Label/Heading: one per remaining block).
const FILES: &[(&str, &str, usize, &[&str])] = &[
    (
        "PendingDecisionSheet.qml",
        include_str!("../qml/PendingDecisionSheet.qml"),
        14,
        &[],
    ),
    (
        "MakeRuleSheet.qml",
        include_str!("../qml/MakeRuleSheet.qml"),
        7,
        &[],
    ),
    (
        "LevelRuleActions.qml",
        include_str!("../qml/LevelRuleActions.qml"),
        3,
        &[],
    ),
    (
        "AlertWindow.qml",
        include_str!("../qml/AlertWindow.qml"),
        0,
        &[
            // Security surface: see the module comment.
            "objectName: \"alertProgram\"",
            "objectName: \"alertPath\"",
            "objectName: \"alertDestination\"",
            "text: \"Applies to\"",
            "text: \"For how long\"",
            "objectName: \"alertOnceOnlyNote\"",
            "objectName: \"alertCountdown\"",
        ],
    ),
    (
        "SizedOverlaySheet.qml",
        include_str!("../qml/SizedOverlaySheet.qml"),
        0,
        &[
            // The sheet title elides (honest_ui_pages_qml asserts a long title
            // really does) and its PlainText tooltip is shown on `truncated`.
            "id: titleHeading",
            // The tooltip's content item (class D).
            "color: palette.toolTipText",
        ],
    ),
    (
        "InspectorPanel.qml",
        include_str!("../qml/InspectorPanel.qml"),
        3,
        &[
            // Elided, with a HoverTip on `truncated`; ConnectionDetailsCopy
            // copies the full values.
            "id: titleLabel",
            "objectName: \"inspectorHost\"",
            // Elided; ConnectionDetailsCopy copies them.
            "Kirigami.FormData.label: \"Destination IP\"",
            "Kirigami.FormData.label: \"Matched rule\"",
        ],
    ),
    (
        "ConnectionExplanation.qml",
        include_str!("../qml/ConnectionExplanation.qml"),
        1,
        &[],
    ),
];

/// (file, [(marker unique to one SelectableText block, its exact wrapMode)]).
/// One entry per converted block, so the table also pins the block count.
const WRAPS: &[(&str, &[(&str, &str)])] = &[
    (
        "PendingDecisionSheet.qml",
        &[
            ("\" wants to connect to \"", "Text.Wrap"),
            ("text: \"Scope\"", "Text.NoWrap"),
            ("text: \"Duration\"", "Text.NoWrap"),
            ("id: denyOnceHintLabel", "Text.Wrap"),
            (
                "so this answer applies only to this connection.",
                "Text.Wrap",
            ),
            ("id: oldBridgeNoteLabel", "Text.Wrap"),
            ("\"If nobody answers within \"", "Text.NoWrap"),
            ("can't put a prompt off", "Text.Wrap"),
            ("\"Reverse DNS\"", "Text.Wrap"),
            ("\"Organization\"", "Text.Wrap"),
            ("\"Registrar\"", "Text.Wrap"),
            ("\"Country\"", "Text.Wrap"),
            ("\"unavailable (offline?)\"", "Text.Wrap"),
            ("\"Online research disabled", "Text.Wrap"),
        ],
    ),
    (
        "MakeRuleSheet.qml",
        &[
            ("id: alsoListedNote", "Text.Wrap"),
            ("so it can't make a rule for it.", "Text.Wrap"),
            ("text: \"Scope\"", "Text.NoWrap"),
            ("text: \"Duration\"", "Text.NoWrap"),
            ("id: blockNoteLabel", "Text.Wrap"),
            ("\"makeRuleBusyElsewhere\"", "Text.Wrap"),
            ("\"makeRuleResult\"", "Text.Wrap"),
        ],
    ),
    (
        "LevelRuleActions.qml",
        &[
            ("\"confirmBody\"", "Text.Wrap"),
            ("\"confirmProgram\"", "Text.WrapAnywhere"),
            ("\"ruleMessage\"", "Text.Wrap"),
        ],
    ),
    ("AlertWindow.qml", &[]),
    ("SizedOverlaySheet.qml", &[]),
    (
        "InspectorPanel.qml",
        &[
            ("\"Port\"", "Text.NoWrap"),
            ("\"Protocol\"", "Text.NoWrap"),
            ("\"Verdict\"", "Text.NoWrap"),
        ],
    ),
    (
        "ConnectionExplanation.qml",
        &[("\"connectionExplanation\"", "Text.WrapAnywhere")],
    ),
];

#[test]
fn batch_7_texts_are_selectable() {
    for (name, source, selectable, kept) in FILES {
        let code = code_lines(source);
        assert_eq!(
            blocks(&code, "SelectableText {").len(),
            *selectable,
            "{name}: SelectableText blocks"
        );
        let mut left = blocks(&code, "Controls.Label {");
        left.extend(blocks(&code, "Kirigami.Heading {"));
        assert_eq!(
            left.len(),
            kept.len(),
            "{name}: plain Label/Heading blocks left:\n{}",
            left.join("\n---\n")
        );
        for snippet in *kept {
            assert!(
                left.iter().any(|b| b.contains(snippet)),
                "{name}: expected a kept Label/Heading containing `{snippet}`"
            );
        }
    }
}

/// The alert window has no selectable text and no row menu at all (not even
/// outside a block the counter above sees): either brings a focus-taking
/// popup and a Ctrl+C shortcut into the prompt.
#[test]
fn batch_7_alert_window_has_no_selectable_text_or_menu() {
    let code = code_lines(include_str!("../qml/AlertWindow.qml"));
    for forbidden in ["SelectableText", "RowCopyMenu", "Controls.Menu", "Shortcut"] {
        assert!(
            !code.contains(forbidden),
            "AlertWindow.qml uses `{forbidden}`: the prompt must not gain a \
             focus-taking popup or a shortcut"
        );
    }
}

#[test]
fn batch_7_has_no_row_menus() {
    for (name, source, _, _) in FILES {
        assert!(
            blocks(&code_lines(source), "RowCopyMenu {").is_empty(),
            "{name}: no clickable rows in this batch, so no RowCopyMenu"
        );
    }
}

/// Types that react to a click. `Button {` also matches `ToolButton {`,
/// `Controls.Button {` and the like; `ItemDelegate {` also matches
/// `Controls.ItemDelegate {`.
const CLICK_TARGETS: &[&str] = &[
    "ItemDelegate {",
    "MouseArea {",
    "TapHandler {",
    "Button {",
    "AbstractListItem {",
    "BasicListItem {",
    "SwipeListItem {",
];

#[test]
fn batch_7_texts_are_not_inside_click_targets() {
    for (name, source, _, _) in FILES {
        let code = code_lines(source);
        for target in CLICK_TARGETS {
            for block in blocks(&code, target) {
                assert!(
                    !block.contains("SelectableText"),
                    "{name}: a SelectableText sits inside `{target}` (a click on it would \
                     be a click on the row; that needs a RowCopyMenu):\n{block}"
                );
            }
        }
        // And none has a click target of its own inside it.
        for text in blocks(&code, "SelectableText {") {
            for target in CLICK_TARGETS {
                assert!(
                    !text.contains(target),
                    "{name}: a SelectableText contains `{target}`:\n{text}"
                );
            }
        }
    }
}

/// Components whose root block is a `SelectableText` (so using one is using a
/// selectable text), derived from `qml/*.qml`.
fn selectable_root_components() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qml");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read qml dir") {
        let path = entry.expect("qml dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("qml") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read qml file");
        let root = code_lines(&source)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("import ") && !l.starts_with("pragma "))
            .map(str::to_owned);
        if root.as_deref() == Some("SelectableText {") {
            names.push(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    names.sort();
    names
}

/// A component whose root is a `SelectableText` is a selectable text: it must
/// not sit inside a click target, and (its own file is checked above through
/// the `SelectableText {` root block) nothing clickable may sit inside its
/// use site either.
#[test]
fn batch_7_selectable_root_components_are_not_inside_click_targets() {
    let components = selectable_root_components();
    assert!(
        components.iter().any(|c| c == "ConnectionExplanation"),
        "root scan lost ConnectionExplanation: {components:?}"
    );
    for (name, source, _, _) in FILES {
        let code = code_lines(source);
        for component in &components {
            let open = format!("{component} {{");
            for target in CLICK_TARGETS {
                for block in blocks(&code, target) {
                    assert!(
                        !block.contains(&open),
                        "{name}: `{component}` (a SelectableText) sits inside `{target}`:\n{block}"
                    );
                }
            }
            for usage in blocks(&code, &open) {
                for target in CLICK_TARGETS {
                    assert!(
                        !usage.contains(target),
                        "{name}: `{component}` contains `{target}`:\n{usage}"
                    );
                }
            }
        }
    }
}

/// A direct `Type { ... }` child of a QML body. `deferred` children are
/// templates (`property Component x:`, `delegate:`, `sourceComponent:`, a
/// `Component {}`), not items instantiated under the parent.
struct Kid {
    name: String,
    body: String,
    deferred: bool,
    /// Bound to `delegate:` (what a `Repeater` instantiates).
    delegate: bool,
}

/// `body` with comments removed and the contents of "..." / '...' strings
/// blanked (quotes kept, escapes honoured), so braces in either are inert.
fn mask_strings_and_comments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '/' if chars.peek() == Some(&'/') => {
                while chars.peek().is_some_and(|c| *c != '\n') {
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
            }
            '"' | '\'' | '`' => {
                out.push(ch);
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        chars.next();
                    } else if c == ch {
                        break;
                    }
                }
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Direct `Type { ... }` children of a QML body (brace counting on the masked
/// text) plus the body's own text, one statement per line, with child blocks
/// removed but a non-type block's header (`onClicked:`) kept. A `prop: Type {`
/// or list-entry (`[A {}, B {}]`) header yields the type. A `Repeater`'s
/// delegates are hoisted: they become children of the Repeater's parent.
fn split_body(body: &str) -> (Vec<Kid>, String) {
    let body = mask_strings_and_comments(body);
    let (mut kids, mut own) = (Vec::new(), String::new());
    let (mut depth, mut seg_start, mut open_at) = (0usize, 0usize, 0usize);
    let mut header = String::new();
    for (i, ch) in body.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    open_at = i;
                    header = body[seg_start..i].to_string();
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let name = header.rsplit([':', '[', ',']).next().unwrap_or("").trim();
                    let is_type = !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '.' || c == '_')
                        && last_segment(name).starts_with(char::is_uppercase);
                    if is_type {
                        let key = header.trim_end().strip_suffix(name).unwrap_or("");
                        let delegate = key.trim_end().ends_with("delegate:");
                        let deferred = key
                            .split_whitespace()
                            .any(|t| t == "Component" || t == "component")
                            || delegate
                            || name == "Component";
                        let inner = body[open_at + 1..i].to_string();
                        if last_segment(name) == "Repeater" {
                            kids.extend(
                                split_body(&inner)
                                    .0
                                    .into_iter()
                                    .filter(|k| k.delegate)
                                    .flat_map(|k| {
                                        if last_segment(&k.name) == "Component" {
                                            split_body(&k.body).0
                                        } else {
                                            vec![k]
                                        }
                                    })
                                    .map(|k| Kid {
                                        deferred: false,
                                        ..k
                                    }),
                            );
                        } else {
                            kids.push(Kid {
                                name: name.to_string(),
                                body: inner,
                                deferred,
                                delegate,
                            });
                        }
                    } else {
                        own.push_str(header.trim());
                        own.push('\n');
                    }
                    seg_start = i + 1;
                }
            }
            '\n' if depth == 0 && body[i + 1..].trim_start().starts_with('{') => {}
            '\n' | ';' if depth == 0 => {
                own.push_str(body[seg_start..i].trim());
                own.push('\n');
                seg_start = i + 1;
            }
            _ => {}
        }
    }
    assert_eq!(depth, 0, "unbalanced braces (a regex literal?):\n{body}");
    own.push_str(body[seg_start.min(body.len())..].trim());
    (kids, own)
}

/// Types that capture presses on their parent item (a sibling of a selectable
/// text, not a container of it). A Button/ToolButton sibling in a layout does
/// not, so those are not listed.
const SIBLING_CAPTURES: &[&str] = &[
    "TapHandler",
    "MouseArea",
    "DragHandler",
    "PointHandler",
    "HoverHandler",
];
/// Types that are themselves clickable, as the PARENT of a selectable text.
const CLICKABLE_PARENTS: &[&str] = &[
    "ItemDelegate",
    "MouseArea",
    "Button",
    "AbstractListItem",
    "BasicListItem",
    "SwipeListItem",
];
const PARENT_CLICK_HANDLERS: &[&str] = &[
    "onClicked:",
    "onTapped:",
    "onPressed:",
    "onDoubleClicked:",
    "onPressAndHold:",
];

fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Every block that directly contains a selectable text (a `SelectableText`
/// or a use of a selectable-root component) must itself not be clickable, have
/// no `onClicked`-style handler of its own, and have no sibling handler child.
fn sibling_capture_violations(
    parent: &str,
    body: &str,
    selectable: &[String],
    out: &mut Vec<String>,
) {
    let (kids, own) = split_body(body);
    let is_text = |n: &str| {
        let l = last_segment(n);
        l == "SelectableText" || selectable.iter().any(|s| s == l)
    };
    let live: Vec<&Kid> = kids.iter().filter(|k| !k.deferred).collect();
    if let Some(text) = live.iter().find(|k| is_text(&k.name)).map(|k| &k.name) {
        if CLICKABLE_PARENTS
            .iter()
            .any(|c| last_segment(parent).ends_with(c))
        {
            out.push(format!("`{text}` sits directly in clickable `{parent}`"));
        }
        for line in own.lines().map(|l| l.replace([' ', '\t'], "")) {
            if PARENT_CLICK_HANDLERS.iter().any(|h| line.starts_with(h)) {
                out.push(format!("`{parent}` holding `{text}` has `{line}`"));
            }
        }
        for k in &live {
            if SIBLING_CAPTURES.contains(&last_segment(&k.name)) {
                out.push(format!(
                    "`{}` is a sibling of `{text}` in `{parent}`",
                    k.name
                ));
            }
        }
    }
    for k in &kids {
        sibling_capture_violations(&k.name, &k.body, selectable, out);
    }
}

#[test]
fn batch_7_texts_have_no_sibling_click_capture() {
    let components = selectable_root_components();
    for (name, source, _, _) in FILES {
        let mut out = Vec::new();
        sibling_capture_violations("<file>", &code_lines(source), &components, &mut out);
        assert!(out.is_empty(), "{name}: {out:#?}");
    }
}

#[test]
fn batch_7_sibling_guard_self_test() {
    let comps = vec!["ConnectionExplanation".to_string()];
    let run = |src: &str| {
        let mut out = Vec::new();
        sibling_capture_violations("<file>", src, &comps, &mut out);
        out
    };
    let bad = [
        "Item {\n    ConnectionExplanation {\n        x: 1\n    }\n    TapHandler {\n        onTapped: page.dismissInspector()\n    }\n}",
        "Item {\n    SelectableText {\n        text: \"a\"\n    }\n    MouseArea {\n        anchors.fill: parent\n    }\n}",
        "Controls.ItemDelegate {\n    contentItem: Item {\n        SelectableText {\n        }\n    }\n    Controls.ItemDelegate {\n        SelectableText {\n        }\n    }\n}",
        "Item {\n    SelectableText {\n    }\n    HoverHandler {\n    }\n}",
        "Item {\n    onClicked: foo()\n    SelectableText {\n    }\n}",
        "Item\n{\n    SelectableText {\n    }\n    TapHandler\n    {\n    }\n}",
        "Item {\n    onClicked  \t: foo()\n    SelectableText {\n    }\n}",
        "Item {\n    property Item componentText: SelectableText {\n    }\n    TapHandler {\n    }\n}",
        "Item {\n    onTapped : foo()\n    SelectableText {\n    }\n}",
        "Item {\n    Repeater {\n        delegate: Component {\n            SelectableText {\n            }\n        }\n    }\n    TapHandler {\n    }\n}",
        "Item {\n    onClicked: { foo() }\n    SelectableText {\n    }\n}",
        "Item {\n    property bool f: true; onClicked: foo()\n    SelectableText {\n    }\n}",
        "Item {\n    property string l: '{'\n    SelectableText {\n    }\n    TapHandler {\n    }\n}",
        "Item {\n    property string l: \"a\\\"{\"\n    SelectableText {\n    }\n    TapHandler {\n    }\n}",
        "Item {\n    data: [SelectableText {}, TapHandler {}]\n}",
        "Item {\n    Repeater {\n        delegate: SelectableText {\n        }\n    }\n    TapHandler {\n    }\n}",
        "Item {\n    /* { */ SelectableText {\n    }\n    TapHandler {\n    } // }\n}",
    ];
    for src in bad {
        assert!(!run(src).is_empty(), "bad fixture not flagged:\n{src}");
    }
    let good = "RowLayout {\n    SelectableText {\n        text: \"a { b\"\n    }\n    Controls.Button {\n        onClicked: { go() }\n    }\n    ConnectionExplanation {\n    }\n}";
    assert_eq!(run(good), Vec::<String>::new());
    // A deferred template is not an instantiated sibling.
    let deferred = "Item {\n    property Component preview: SelectableText {\n    }\n    TapHandler {\n    }\n}";
    assert_eq!(run(deferred), Vec::<String>::new());
    let unrelated = "Item {\n    Repeater {\n        delegate: Item {\n        }\n        property Component p: SelectableText {\n        }\n    }\n    TapHandler {\n    }\n}";
    assert_eq!(run(unrelated), Vec::<String>::new());
    let inline =
        "Item {\n    component MyText: SelectableText {\n    }\n    TapHandler {\n    }\n}";
    assert_eq!(run(inline), Vec::<String>::new());
}

#[test]
fn batch_7_adds_no_tab_stop() {
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            for forbidden in [
                "activeFocusOnTab",
                "focus:",
                "KeyNavigation",
                "forceActiveFocus",
                "activeFocusOnPress",
            ] {
                assert!(
                    !block.contains(forbidden),
                    "{name}: a SelectableText sets `{forbidden}` (mouse only):\n{block}"
                );
            }
        }
    }
}

#[test]
fn batch_7_text_stays_explicitly_plain() {
    for (name, source, _, _) in FILES {
        for block in blocks(&code_lines(source), "SelectableText {") {
            let formats: Vec<&str> = block
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("textFormat:"))
                .collect();
            assert_eq!(
                formats,
                ["textFormat: Text.PlainText"],
                "{name}: a SelectableText must set exactly `textFormat: Text.PlainText`:\n{block}"
            );
        }
    }
}

#[test]
fn batch_7_keeps_layout_equivalent() {
    for (name, source, _, _) in FILES {
        let converted = blocks(&code_lines(source), "SelectableText {");
        for block in &converted {
            assert!(
                !block.contains("elide:"),
                "{name}: a SelectableText sets `elide` (a TextEdit cannot elide):\n{block}"
            );
        }
        let (_, expected) = WRAPS
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name}: missing from WRAPS"));
        assert_eq!(
            converted.len(),
            expected.len(),
            "{name}: WRAPS entries vs blocks"
        );
        for (marker, wrap) in *expected {
            let hits: Vec<&String> = converted.iter().filter(|b| b.contains(marker)).collect();
            assert_eq!(
                hits.len(),
                1,
                "{name}: marker `{marker}` must match one block"
            );
            let want = format!("wrapMode: {wrap}");
            let wraps: Vec<&str> = hits[0]
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("wrapMode:"))
                .collect();
            assert_eq!(
                wraps,
                [want.as_str()],
                "{name}: block `{marker}` must have exactly `{want}`:\n{}",
                hits[0]
            );
        }
    }
}
