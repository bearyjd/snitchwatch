//! The qmltestrunner suite (`tests/qml/tst_*.qml`, run by CI and `just
//! qml-test`) loads production QML through `Production.X`, but cannot see
//! the types cxx-qt registers from Rust. Every Rust-registered type reachable
//! from a production file a test loads must have a stub in
//! `tests/qml/imports/com/snitchwatch/shell/qmldir`; `cargo test` does not run
//! qmltestrunner, so this is the local tripwire for the CI-only failure
//! "Type X unavailable".
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

/// Registered types that are fine un-stubbed in the qmltestrunner import
/// dir. Each needs a one-line reason; prefer a stub over an entry here.
const ALLOWED_UNSTUBBED: &[(&str, &str)] = &[];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Strips `/* */` and `//` comments (QML strings here never contain `//`).
fn strip_comments(source: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find("*/").map_or("", |j| &rest[i + j + 2..]);
    }
    out.push_str(rest);
    out.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tokens(source: &str) -> BTreeSet<String> {
    strip_comments(source)
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Type names the app registers into `com.snitchwatch.shell`: cxx-qt
/// `#[qml_element]` bridge types (`src/*.rs`) and the `.singleton(true)`
/// `QmlFile`s in `build.rs` (a directory import cannot see a
/// `pragma Singleton` without a qmldir entry).
fn registered_types() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for entry in fs::read_dir(root().join("src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("qml_name"),
            "{}: #[qml_name] renames a type; teach registered_types() about it",
            path.display()
        );
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.trim() != "#[qml_element]" {
                continue;
            }
            let decl = lines[i + 1..]
                .iter()
                .map(|l| l.trim())
                .find(|l| l.starts_with("type "))
                .unwrap_or_else(|| {
                    panic!("{}:{}: no type after qml_element", path.display(), i + 1)
                });
            let name = decl["type ".len()..].split_whitespace().next().unwrap();
            found.insert(name.to_owned());
        }
    }
    let build = fs::read_to_string(root().join("build.rs")).unwrap();
    for part in build.split("QmlFile::from(\"qml/").skip(1) {
        let (file, tail) = part.split_once('"').unwrap();
        if tail.trim_start_matches(')').starts_with(".singleton(true)") {
            found.insert(file.trim_end_matches(".qml").to_owned());
        }
    }
    found
}

/// Declaration rows of a qmldir (`[singleton] Name Version File`), with
/// comments removed. Returns (name, file) pairs.
fn qmldir_rows(qmldir: &str) -> Vec<(String, String)> {
    qmldir
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter_map(|l| {
            let mut w: Vec<&str> = l.split_whitespace().collect();
            if w.first() == Some(&"singleton") {
                w.remove(0);
            }
            match w.as_slice() {
                [name, version, file]
                    if version.chars().all(|c| c.is_ascii_digit() || c == '.')
                        && !["module", "plugin", "import", "depends"].contains(name) =>
                {
                    Some((name.to_string(), file.to_string()))
                }
                _ => None,
            }
        })
        .collect()
}

/// Production QML files transitively referenced by the `Production.X` types
/// the qmltestrunner tests load.
fn reachable_production_types() -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = Vec::new();
    for entry in fs::read_dir(root().join("tests/qml")).unwrap() {
        let path = entry.unwrap().path();
        let is_test = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("tst_") && n.ends_with(".qml"));
        if !is_test {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for part in text.split("Production.").skip(1) {
            let name: String = part
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            todo.push(name);
        }
    }
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Ok(text) = fs::read_to_string(root().join("qml").join(format!("{name}.qml"))) else {
            continue;
        };
        todo.extend(tokens(&text));
    }
    seen
}

#[test]
fn qml_test_import_dir_stubs_every_rust_type_the_tests_reach() {
    let dir = root().join("tests/qml/imports/com/snitchwatch/shell");
    let rows = qmldir_rows(&fs::read_to_string(dir.join("qmldir")).unwrap());
    for (name, file) in &rows {
        assert!(
            dir.join(file).is_file(),
            "qmldir stub {name} points at missing file {file}"
        );
    }
    let declared: BTreeSet<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
    let registered = registered_types();
    assert!(registered.contains("NativeClipboard"), "{registered:?}");
    assert!(registered.contains("SelectionRegistry"), "{registered:?}");
    let reached = reachable_production_types();
    assert!(
        reached.contains("MapView"),
        "walk found no tests: {reached:?}"
    );
    for ty in registered.intersection(&reached) {
        let allowed = ALLOWED_UNSTUBBED.iter().any(|(n, _)| n == ty);
        assert!(
            declared.contains(ty.as_str()) || allowed,
            "{ty} is registered from Rust and reachable from tests/qml/tst_*.qml but has no qmldir stub in the qmltestrunner import dir"
        );
    }
}

#[test]
fn qmldir_rows_ignore_comments() {
    let rows = qmldir_rows(
        "module m\n# NativeClipboard 1.0 A.qml\nsingleton S 1.0 S.qml # x\nB 1.0 B.qml\n",
    );
    assert_eq!(
        rows,
        vec![("S".into(), "S.qml".into()), ("B".into(), "B.qml".into())]
    );
}
