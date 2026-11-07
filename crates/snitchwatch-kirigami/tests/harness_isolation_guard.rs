//! Every integration test that runs app code in-process (a QML engine, a Qt
//! application, the settings or learned list, the bridge runtime) reads and
//! writes `~/.config/snitchwatch` and the runtime dir. Each such file must pull
//! in `tests/common/mod.rs`, whose constructor moves HOME and the XDG dirs to a
//! throwaway directory before `main` (see `home_isolation`). A file that
//! forgets `mod common;` would silently run against the developer's real files.
//!
//! Coverage is exactly the markers below, over `tests/*.rs` and
//! `tests/*/main.rs`; a test that reaches app code by another route is not
//! seen by this guard.

use std::path::Path;

/// Any of these in a test file means it runs app code in-process.
const MARKERS: [&str; 11] = [
    "QQmlApplicationEngine",
    "QQmlEngine",
    "QGuiApplication",
    "QApplication",
    "probe_runner",
    "SettingsController",
    "learned::",
    "ensure_started",
    "bridge_runtime::",
    "TestLink",
    "BridgeConfig",
];

/// Files that mention a marker but only as text (source scans) or never start
/// app code. One reason each; keep this list small.
const ALLOWED: [(&str, &str); 3] = [
    (
        "qml_source_guards.rs",
        "scans QML/Rust sources as text, runs nothing",
    ),
    (
        "live_loop.rs",
        "starts a bridge, but its socket and token live in an explicit tempdir BridgeConfig",
    ),
    (
        "rule_hits_live.rs",
        "starts a bridge, but its socket and token live in an explicit tempdir BridgeConfig",
    ),
];

/// The code lines of `source`: `//` lines and whole-line `/* */` blocks are
/// dropped. A quoted `mod common;` is not a declaration either: it has to
/// start its own line.
fn code_lines(source: &str) -> Vec<String> {
    let mut in_block = false;
    let mut lines = Vec::new();
    for line in source.lines().map(str::trim) {
        if in_block || line.starts_with("/*") {
            in_block = !line.contains("*/");
        } else if !line.starts_with("//") {
            lines.push(line.to_string());
        }
    }
    lines
}

fn declares_common(source: &str) -> bool {
    code_lines(source).iter().any(|line| {
        line == "mod common;" || (line.starts_with("#[path") && line.ends_with("mod common;"))
    })
}

fn lacks_isolation(source: &str) -> bool {
    MARKERS.iter().any(|m| source.contains(m)) && !declares_common(source)
}

fn test_sources(dir: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("tests dir") {
        let path = entry.expect("dir entry").path();
        let file = if path.is_dir() {
            path.join("main.rs")
        } else {
            path.clone()
        };
        let name = file
            .strip_prefix(dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if file.extension().is_none_or(|e| e != "rs") || !file.is_file() {
            continue;
        }
        found.push((
            name,
            std::fs::read_to_string(&file).expect("read test source"),
        ));
    }
    found
}

#[test]
fn every_test_that_runs_app_code_uses_the_isolated_home() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut missing: Vec<String> = test_sources(&dir)
        .into_iter()
        .filter(|(name, source)| {
            name != "harness_isolation_guard.rs"
                && !ALLOWED.iter().any(|(allowed, _)| allowed == name)
                && lacks_isolation(source)
        })
        .map(|(name, _)| name)
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "add `mod common;` (tests/common/mod.rs isolates HOME and the XDG dirs) to: {missing:?}"
    );
}

#[test]
fn a_commented_out_or_quoted_mod_common_does_not_count() {
    let app = "use cxx_qt_lib::QGuiApplication;\n";
    assert!(lacks_isolation(&format!("{app}// mod common;\n")));
    assert!(lacks_isolation(&format!("{app}/* mod common; */\n")));
    assert!(lacks_isolation(&format!(
        "{app}const S: &str = \"mod common;\";\n"
    )));
    assert!(!lacks_isolation(&format!("{app}mod common;\n")));
    assert!(!lacks_isolation(&format!(
        "{app}#[path = \"x/mod.rs\"] mod common;\n"
    )));
    assert!(!lacks_isolation("fn pure_logic() {}\n"));
}

#[test]
fn the_new_markers_are_caught() {
    for marker in [
        "QApplication",
        "SettingsController",
        "learned::",
        "TestLink",
    ] {
        assert!(lacks_isolation(&format!("let _ = {marker};")), "{marker}");
    }
}
