//! Integration smoke: `BlocklistsModel` / `BlocklistEntriesModel` are usable
//! as QML elements (Task 9).
//!
//! Scope of this test — and, deliberately, only this (mirrors
//! `connections_model_qml.rs`'s scope note):
//!   * both cxx-qt wrappers register as QML types under `com.snitchwatch.shell`
//!     and instantiate (a null root object would mean a type failed to
//!     register or compile against its `QAbstractListModel` base), and
//!   * `applyServerMessageJson` is callable from QML and drives the real
//!     deserialize -> row-store-apply path without aborting (a Rust panic
//!     inside the invokable would abort this test binary), and
//!   * `subscribe`/`unsubscribe` run the real emit-signal path without
//!     aborting.
//!
//! It intentionally does NOT assert row counts/content: a thrown JS error in
//! `Component.onCompleted` does not null the root object, so QML-side asserts
//! are not load-bearing. The models' ordering/CRUD *correctness* is covered
//! exhaustively and Qt-free by the `blocklists::row_store` unit tests, which
//! are what Task 9's acceptance criterion calls for. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::blocklists_model as _;

#[test]
fn blocklists_model_registers_and_ingest_invokable_runs() {
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        std::env::set_var("QT_QPA_PLATFORM", "offscreen");
    }
    if std::env::var_os("QT_QUICK_CONTROLS_STYLE").is_none() {
        std::env::set_var("QT_QUICK_CONTROLS_STYLE", "Basic");
    }

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();

    let root_ok = Arc::new(AtomicBool::new(false));

    // Instantiate both Rust models and drive one real event through each:
    // a SetBlocklists insert on the subscriptions model, a SetBlocklistEntries
    // insert on the entries model, plus one subscribe/unsubscribe call each.
    // If any invokable panicked, the whole test binary would abort; if a type
    // were unregistered, the root would be null.
    let qml = r#"
import QtQuick
import com.snitchwatch.shell

QtObject {
    property BlocklistsModel model: BlocklistsModel {}
    property BlocklistEntriesModel entries: BlocklistEntriesModel {}
    Component.onCompleted: {
        model.applyServerMessageJson(JSON.stringify({
            action: "setBlocklists",
            blocklists: [
                { id: "l1", displayName: "Ads", url: "https://example.invalid/ads.txt",
                  entryCount: 3, status: "ok", lastUpdatedIso8601: null, lastFailureReason: null }
            ]
        }));
        entries.applyServerMessageJson(JSON.stringify({
            action: "setBlocklistEntries",
            subscriptionId: "l1",
            entries: [ { host: "ads.example" }, { host: "tracker.example" } ]
        }));
        model.subscribe("https://example.invalid/new.txt");
        model.unsubscribe("l1");
        entries.clearEntries();
        // Best-effort visibility only (not load-bearing — see module docs).
        console.log("[test] BlocklistsModel.count after insert =", model.count,
                    "entries.count =", entries.count);
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
            &QUrl::from("qrc:/inline_blocklists_probe.qml"),
        );
    }
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "BlocklistsModel/BlocklistEntriesModel QML probe failed: root object was null — a \
         type failed to register as a QML element or did not compile against its \
         QAbstractListModel base"
    );

    let _ = app.as_mut();
}
