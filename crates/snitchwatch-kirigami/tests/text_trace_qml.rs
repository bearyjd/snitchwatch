//! `SNITCHWATCH_TEXT_TRACE` guards (docs/superpowers/plans/2026-11-03-text-trace.md):
//! main.qml mounts the tracer only behind `appInfo.textTrace`, so with the
//! flag off nothing is instantiated; the file is registered in build.rs; and
//! main.qml stays under the 800-line cap.
mod qml_guard_support;

use qml_guard_support::{blocks, code_lines};

const MAIN: &str = include_str!("../qml/main.qml");
const BUILD: &str = include_str!("../build.rs");
const TRACE: &str = include_str!("../qml/TextTrace.qml");

#[test]
fn main_qml_mounts_the_tracer_only_behind_the_flag() {
    let code = code_lines(MAIN);
    assert_eq!(code.matches("TextTrace {").count(), 1, "one tracer mount");
    let loaders = blocks(&code, "Loader {");
    let mount: Vec<_> = loaders
        .iter()
        .filter(|b| b.contains("TextTrace {"))
        .collect();
    assert_eq!(mount.len(), 1, "the tracer must sit in a Loader");
    assert!(
        mount[0].contains("active: appInfo.textTrace"),
        "the Loader is not gated on appInfo.textTrace"
    );
    assert!(
        mount[0].contains("trayController.tooltip"),
        "tray tooltip not traced"
    );
    assert!(
        MAIN.lines().count() <= 800,
        "main.qml is {} lines",
        MAIN.lines().count()
    );
}

#[test]
fn tracer_is_registered_and_never_logs_without_being_asked() {
    assert!(
        BUILD.contains("\"qml/TextTrace.qml\""),
        "not registered in build.rs"
    );
    // Change-driven: no polling timer.
    assert!(
        !code_lines(TRACE).contains("Timer {"),
        "tracer must not poll"
    );
}
