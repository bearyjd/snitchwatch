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

/// Types the production QML uses that exist only as Rust registrations (or
/// that the stub module must re-export). Extend when a new one is needed.
const STUBBED: &[&str] = &["NativeClipboard", "SelectionRegistry"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tokens(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .flat_map(|l| l.split(|c: char| !c.is_alphanumeric() && c != '_'))
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
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
    let qmldir =
        fs::read_to_string(root().join("tests/qml/imports/com/snitchwatch/shell/qmldir")).unwrap();
    let declared = tokens(&qmldir);
    let reached = reachable_production_types();
    assert!(
        reached.contains("MapView"),
        "walk found no tests: {reached:?}"
    );
    for ty in STUBBED {
        if reached.contains(*ty) {
            assert!(declared.contains(*ty), "{ty} is reachable from tests/qml/tst_*.qml but missing from the qmltestrunner qmldir stub");
        }
    }
}
