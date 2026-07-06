//! Integration smoke: `RulesPage.qml`'s rule-match diagnostics additions —
//! the "Show rule" jump target (`openRuleByName`) and the Simulate panel
//! (`runSimulation`, backed by `RulesModel::simulate` ->
//! `rules::simulator::simulate`).
//!
//! Scope of this test — same convention as `rules_model_qml.rs`:
//!   * `RulesPage.qml` compiles and instantiates directly as a QML type
//!     under `com.snitchwatch.shell` (a null root object would mean a
//!     type/property error in the file), and
//!   * `openRuleByName`/`runSimulation` run without a Rust panic aborting
//!     the test binary.
//!
//! As with the existing model smokes, a thrown JS error inside
//! `Component.onCompleted` does not null the root object, so this does not
//! assert on the simulator's/inspector's *content* — that correctness is
//! covered exhaustively and Qt-free by `rules::simulator`'s unit tests. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

#[test]
fn rules_page_open_rule_by_name_and_simulate_run_without_erroring() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import com.snitchwatch.shell

RulesPage {
    id: page
    model: RulesModel {
        id: rulesModel
        Component.onCompleted: {
            applyServerMessageJson(JSON.stringify({
                action: "setRules",
                rules: [
                    { name: "899-curl-allow", enabled: true, action: "allow",
                      duration: "always", description: "",
                      operator: { operand: "process.path", data: "/usr/bin/curl" } }
                ]
            }));
        }
    }
    Component.onCompleted: {
        // "Show rule" jump target: known name, then an unknown one (should
        // just return false, not throw).
        page.openRuleByName("899-curl-allow");
        page.openRuleByName("does-not-exist");
        // Simulate panel, backed by the real RulesModel.simulate qinvokable.
        page.runSimulation();
        console.log("[test] simulateMatchedRule =", page.simulateMatchedRule);
    }
}
"#;

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    if let Some(engine) = engine.as_mut() {
        engine.load_data(
            &QByteArray::from(qml),
            &QUrl::from("qrc:/inline_rules_page_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "RulesPage QML probe failed: root object was null — a type/property error in \
         RulesPage.qml (openRuleByName/runSimulation/Simulate sheet)"
    );

    let _ = app.as_mut();
}
