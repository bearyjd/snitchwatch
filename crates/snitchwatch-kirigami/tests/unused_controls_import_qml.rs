//! A QML file that imports `QtQuick.Controls as <alias>` must use `<alias>.`
//! somewhere. After the selectable-text / restyle conversions many files kept
//! the import with nothing left that needs it; an unused import is dead weight
//! that makes the next reader hunt for a Controls dependency that is not there.
//!
//! Source guard over `qml/` (comments and string literals are ignored, so a
//! mention in prose does not count as a use). Known limit: an unaliased
//! `import QtQuick.Controls` is not checked.

/// `src` without `//` and `/* */` comments and with string (incl. backtick
/// template text, but not `${}` expressions) contents blanked.
fn code_only(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                while chars.peek().is_some_and(|n| *n != '\n') {
                    chars.next();
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(n) = chars.next() {
                    if n == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        break;
                    }
                }
            }
            '`' => {
                // Literal text is blanked; `${ expr }` is code and is kept.
                out.push(c);
                while let Some(n) = chars.next() {
                    if n == '\\' {
                        chars.next();
                    } else if n == '`' {
                        break;
                    } else if n == '$' && chars.peek() == Some(&'{') {
                        out.push_str(" ${");
                        chars.next();
                        let mut depth = 1;
                        for e in chars.by_ref() {
                            depth += i32::from(e == '{') - i32::from(e == '}');
                            if depth == 0 {
                                break;
                            }
                            out.push(e);
                        }
                        out.push_str("} ");
                    }
                }
                out.push(c);
            }
            '"' | '\'' => {
                out.push(c);
                while let Some(n) = chars.next() {
                    if n == '\\' {
                        chars.next();
                    } else if n == c {
                        break;
                    }
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Aliases from `import QtQuick.Controls as <alias>` that are never used as
/// `<alias>.` in the rest of the file.
fn unused_controls_aliases(src: &str) -> Vec<String> {
    let code = code_only(src);
    let mut unused = Vec::new();
    for line in code.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let ["import", "QtQuick.Controls", .., "as", alias] = words[..] else {
            continue;
        };
        let needle = format!("{alias}.");
        let used = code.lines().filter(|l| l.trim() != line.trim()).any(|l| {
            l.match_indices(&needle)
                .any(|(at, _)| !l[..at].chars().next_back().is_some_and(is_ident_char))
        });
        if !used {
            unused.push(alias.to_string());
        }
    }
    unused
}

#[test]
fn the_unused_import_guard_flags_only_an_unused_alias() {
    let bad = [
        "import QtQuick\nimport QtQuick.Controls as Controls\n\nItem {}\n",
        // Only prose, a string and a lookalike identifier mention it.
        "import QtQuick.Controls as Controls\n// Controls.Button is gone\nItem { text: \"Controls.Label\"; x: MyControls.y }\n",
        "import QtQuick.Controls as C\n/* C.Button */\nItem {}\n",
        "import QtQuick.Controls   as   Controls\nItem {}\n",
        "import QtQuick.Controls 2.15 as Controls\nItem {}\n",
        "import QtQuick.Controls as Controls\nItem { property string s: `Controls.Button` }\n",
    ];
    for src in bad {
        assert_eq!(unused_controls_aliases(src).len(), 1, "should flag:\n{src}");
    }
    let good = [
        "import QtQuick.Controls as Controls\nControls.Button {}\n",
        "import QtQuick.Controls as Controls\nItem { property var p: Controls.Label }\n",
        "import QtQuick.Controls as C\nItem { C.Button {} }\n",
        "import QtQuick\nItem {}\n",
        "import QtQuick.Controls as Controls\nItem { property string s: `x`; property var p: Controls.Popup.CloseOnEscape }",
        "import QtQuick.Controls as Controls\nItem { property string s: `a ${Controls.Popup.CloseOnEscape} b` }\n",
        "import QtQuick.Controls.impl as Impl\nItem {}\n",
    ];
    for src in good {
        assert!(
            unused_controls_aliases(src).is_empty(),
            "should accept:\n{src}"
        );
    }
}

#[test]
fn no_qml_file_imports_controls_without_using_it() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qml");
    let (mut violations, mut with_import) = (Vec::new(), 0);
    for entry in std::fs::read_dir(&dir).expect("qml dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("qml") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("qml source");
        if src.contains("import QtQuick.Controls as ") {
            with_import += 1;
        }
        for alias in unused_controls_aliases(&src) {
            violations.push(format!("{} imports Controls as {alias}", path.display()));
        }
    }
    assert!(
        with_import > 30,
        "only {with_import} files import QtQuick.Controls in {}",
        dir.display()
    );
    assert!(
        violations.is_empty(),
        "unused `import QtQuick.Controls as ...` (remove it):\n{}",
        violations.join("\n")
    );
}
