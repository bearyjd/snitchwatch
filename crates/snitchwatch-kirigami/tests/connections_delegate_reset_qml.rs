//! The production ConnectionList must accept shrinking model resets while a
//! frame's cache incubation is unfinished. A positive control enables Qt's
//! default prefetch on the same real page/model and must reproduce the Qt
//! warning, so both the timing setup and stderr capture are load-bearing.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
#[path = "../src/delegate_incubation_test.rs"]
mod incubation;

const PROBE_URL: &str = "qrc:/connections_delegate_reset_probe.qml";
const DONE: &str = "DELEGATE_RESET_DONE";

#[test]
fn production_list_preserves_navigation_across_shrinks_without_stale_cancellation() {
    common::init_headless_qt_env();
    std::env::remove_var("SNITCHWATCH_DEMO");
    let mut app = QGuiApplication::new();

    for positive_control in [true, false] {
        let mut engine = QQmlApplicationEngine::new();
        let held = incubation::ffi::hold_incubation(engine.pin_mut());
        let root_ok = Arc::new(AtomicBool::new(false));
        let root_created = root_ok.clone();
        let guard = engine
            .pin_mut()
            .on_object_created(move |_engine, obj, _url| {
                root_created.store(!obj.is_null(), Ordering::SeqCst);
            });
        let qml = include_str!("qml/connections_delegate_reset.qml").replace(
            "POSITIVE_CONTROL",
            if positive_control { "true" } else { "false" },
        );
        let captured = common::capture_stderr(|| {
            engine
                .pin_mut()
                .load_data(&QByteArray::from(qml.as_str()), &QUrl::from(PROBE_URL));
            // A failed load has no timer to quit the event loop.
            if root_ok.load(Ordering::SeqCst) {
                app.as_mut().expect("QGuiApplication").exec();
            }
        });
        drop(guard);
        assert!(root_ok.load(Ordering::SeqCst), "QML root: {captured}");
        assert!(captured.contains(DONE), "probe did not finish: {captured}");
        let errors: Vec<_> = captured
            .lines()
            .filter(|line| {
                line.contains(PROBE_URL)
                    || line.contains("TypeError")
                    || line.contains("ReferenceError")
                    || line.contains("DELEGATE_RESET_FAILURE")
            })
            .collect();
        assert!(
            errors.is_empty(),
            "navigation/model fixture failed: {}",
            errors.join("\n")
        );
        let stale_cancel = captured.contains("DelegateModel::cancel: index out range");
        if positive_control {
            assert!(
                held.max_pending() > 0,
                "positive control never incubated: {captured}"
            );
            assert!(
                stale_cancel,
                "positive control did not reproduce Qt's stale cancellation: {captured}"
            );
        } else {
            assert!(
                !stale_cancel,
                "production list cancelled a stale delegate index: {captured}"
            );
        }
    }
}
