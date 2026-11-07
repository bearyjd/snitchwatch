//! Every integration test that starts a QML engine runs the app's own code
//! (settings, learned list, bridge socket) in-process. Those read and write
//! `~/.config/snitchwatch` and the runtime dir, so each such file must pull in
//! `tests/common/mod.rs`, whose constructor moves HOME and the XDG dirs to a
//! throwaway directory before `main` (see `home_isolation`). A file that
//! forgets `mod common;` would silently run against the developer's real files.

use std::path::Path;

/// Any of these in a test file means it runs app code with a QML engine.
const ENGINE_MARKERS: [&str; 4] = [
    "QQmlApplicationEngine",
    "QQmlEngine",
    "QGuiApplication",
    "probe_runner",
];

fn unisolated(dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(dir).expect("tests dir") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.extension().is_none_or(|e| e != "rs") || name == "harness_isolation_guard.rs" {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read test source");
        let runs_engine = ENGINE_MARKERS.iter().any(|m| source.contains(m));
        if runs_engine && !source.contains("mod common;") {
            missing.push(name);
        }
    }
    missing.sort();
    missing
}

#[test]
fn every_test_that_starts_a_qml_engine_uses_the_isolated_home() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let missing = unisolated(&dir);
    assert!(
        missing.is_empty(),
        "add `mod common;` (tests/common/mod.rs isolates HOME and the XDG dirs) to: {missing:?}"
    );
}
