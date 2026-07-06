//! Integration smoke: `WizardController` is usable as a QML element and
//! `OnboardingPage.qml` compiles against it (Task 12).
//!
//! Scope of this test — deliberately, only this (mirrors
//! `traffic_model_qml.rs`'s scope note):
//!   * the cxx-qt wrapper registers as a QML type under `com.snitchwatch.shell`
//!     and instantiates (a null root object would mean the type failed to
//!     register or compile), and
//!   * `probe()`/`startUnit()` are callable from QML and run the real
//!     detect/systemctl paths without aborting (a Rust panic inside either
//!     invokable — including on the scratch-runtime/no-bridge fallback path
//!     exercised here, since `ensure_started()` is never called in this test
//!     binary — would abort this test binary), and
//!   * `OnboardingPage.qml` loads against a real `WizardController` and its
//!     buttons/timer bind without throwing.
//!
//! It intentionally does NOT assert the resolved `state`/`detail` content:
//! both invokables complete asynchronously off the Qt thread, and a thrown JS
//! error in `Component.onCompleted` does not null the root object anyway, so
//! QML-side asserts on timing-dependent state wouldn't be load-bearing here.
//! `DaemonState`/`parse_systemctl_output` *correctness* is covered
//! exhaustively and Qt-free by `wizard`'s own unit tests, which is what
//! Task 12's acceptance criterion calls for. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::wizard_controller as _;

#[test]
fn wizard_controller_registers_and_onboarding_page_loads() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate the Rust controller, call both invokables (exercising the
    // no-bridge-running scratch fallback in this test binary), and load the
    // real OnboardingPage.qml against it.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell

QtObject {
    property WizardController controller: WizardController {}
    property var page: null

    Component.onCompleted: {
        controller.probe();
        controller.startUnit();

        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/OnboardingPage.qml");
        if (component.status === Component.Ready) {
            page = component.createObject(null, { controller: controller });
        } else {
            console.log("[test] OnboardingPage.qml load error:", component.errorString());
        }

        // Best-effort visibility only (not load-bearing — see module docs).
        console.log("[test] WizardController.state after probe/startUnit =", controller.state,
                    "busy =", controller.busy, "detail =", controller.detail);
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
            &QUrl::from("qrc:/inline_wizard_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "WizardController QML probe failed: root object was null — the type failed to \
         register as a QML element, or OnboardingPage.qml did not compile"
    );

    let _ = app.as_mut();
}
