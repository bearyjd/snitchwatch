//! `SNITCHWATCH_TEXT_TRACE` against the real `main.qml` (the stub-tree QML test
//! cannot see the Loader, the `appInfo.textTrace` binding or the real item
//! tree). Same shape as `honest_ui_main_banner_qml.rs`: no event loop, the
//! environment is set BEFORE `main.qml` creates its `AppInfo`, stderr is
//! captured and any QML warning or error fails the test.
//!
//! Flag ON: the Loader's `item` exists and, after the bridge banner's text is
//! changed, a manual `sample()` pair logs the change (frames are not rendered
//! without an event loop, so the `frameSwapped` trigger itself is covered by
//! `tests/qml/tst_text_trace.qml`). Flag OFF: the Loader's `item` is null.
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const ON_URL: &str = "qrc:/text_trace_main_on_probe.qml";
const OFF_URL: &str = "qrc:/text_trace_main_off_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";
const BAD_WORDS: [&str; 5] = [
    "ReferenceError",
    "TypeError",
    "is not defined",
    "unavailable",
    "Error:",
];

fn load(
    mut engine: Pin<&mut QQmlApplicationEngine>,
    root_ok: &AtomicBool,
    qml: &str,
    url: &str,
) -> bool {
    root_ok.store(false, Ordering::SeqCst);
    engine
        .as_mut()
        .load_data(&QByteArray::from(qml), &QUrl::from(url));
    root_ok.load(Ordering::SeqCst)
}

fn bad_lines(captured: &str) -> Vec<&str> {
    captured
        .lines()
        .filter(|l| {
            l.contains(ON_URL)
                || l.contains(OFF_URL)
                || l.contains(SHELL_QML_PREFIX)
                || BAD_WORDS.iter().any(|w| l.contains(w))
        })
        .collect()
}

#[test]
fn main_qml_mounts_the_tracer_only_with_the_flag_and_it_logs() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let (mut on_loaded, mut off_loaded) = (false, false);
    let mut on_out = String::new();
    let mut off_out = String::new();
    if let Some(mut engine) = engine.as_mut() {
        std::env::set_var("SNITCHWATCH_TEXT_TRACE", "1");
        on_out = capture_stderr(|| {
            on_loaded = load(
                engine.as_mut(),
                &root_ok,
                &format!("{PROBE_HEAD}{PROBE_ON_BODY}"),
                ON_URL,
            );
        });
        std::env::remove_var("SNITCHWATCH_TEXT_TRACE");
        off_out = capture_stderr(|| {
            off_loaded = load(
                engine.as_mut(),
                &root_ok,
                &format!("{PROBE_HEAD}{PROBE_OFF_BODY}"),
                OFF_URL,
            );
        });
    }
    drop(guard);

    assert!(on_loaded, "flag-on probe failed to load");
    assert!(off_loaded, "flag-off probe failed to load");
    for (name, out) in [("on", &on_out), ("off", &off_out)] {
        let bad = bad_lines(out);
        assert!(
            bad.is_empty(),
            "flag {name}: QML warnings:\n{}",
            bad.join("\n")
        );
        assert!(
            out.contains(&format!("probe-done {name}")),
            "flag {name}: probe did not finish:\n{out}"
        );
    }
    assert!(
        on_out.contains("text-trace t=")
            && on_out.contains("bridgeMessage-connecting")
            && on_out.contains("Snitchwatch is connecting to its background service"),
        "flag on: no trace line for the allowlisted bridge banner:\n{on_out}"
    );
    for page in ["connections", "rules", "profiles", "blocklists"] {
        assert!(
            on_out.contains(&format!("probe-title-ok {page} ")),
            "{page}: fixed title heading not traced:\n{on_out}"
        );
    }
    assert!(
        !on_out.contains("TITLECANARY"),
        "flag on: text typed into a header input was logged:\n{on_out}"
    );
    assert!(
        !on_out.contains("STATUS_CANARY"),
        "flag on: the unmarked bridge status text was logged:\n{on_out}"
    );
    assert!(
        !off_out.contains("text-trace"),
        "flag off: trace output appeared:\n{off_out}"
    );
    let _ = app.as_mut();
}

/// Creates the real `main.qml` and finds the Loader by objectName.
const PROBE_HEAD: &str = r#"
import QtQuick

QtObject {
    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; i++) {
            const hit = find(item.children[i], name);
            if (hit) return hit;
        }
        return null;
    }

    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml");
        if (component.status !== Component.Ready) {
            throw new Error("main.qml: " + component.errorString());
        }
        const main = component.createObject(null, {});
        if (main === null) {
            throw new Error("main.qml was not created");
        }
        const loader = find(main.contentItem, "textTraceLoader");
        if (loader === null) {
            throw new Error("the Loader is missing");
        }
"#;

const PROBE_ON_BODY: &str = r#"
        if (loader.item === null) {
            throw new Error("the TextTrace instance does not exist with the flag on");
        }
        if (loader.item.titleToolbar() === null) {
            throw new Error("the window title toolbar is not traced for a fixed-title page");
        }
        main.bridgeFeedRef.ok = false;
        main.bridgeFeedRef.linkState = "connecting";
        main.bridgeFeedRef.statusText = "before";
        loader.item.sample();
        main.bridgeFeedRef.linkState = "retrying";
        main.bridgeFeedRef.statusText = "STATUS_CANARY";
        loader.item.sample();
        // The marked areas must be reachable by the walk in the REAL window.
        const keys = Object.keys(loader.item.previous).join("\n");
        ["DefaultPageTitleDelegate", "[bridgeMessage-connecting]", "PromptSlotBanner", "LearningBanner"]
            .forEach(function (part) {
                if (keys.indexOf(part) < 0) {
                    throw new Error("not reachable by the walk: " + part);
                }
            });
        // Pages with a custom titleDelegate: the fixed heading is traced, the
        // adjacent input never (real pages, real header delegates).
        function findBy(item, prop, part) {
            if (typeof item[prop] === "string" && item[prop].indexOf(part) >= 0) return item;
            for (let i = 0; i < item.children.length; i++) {
                const hit = findBy(item.children[i], prop, part);
                if (hit) return hit;
            }
            return null;
        }
        const pg = main.globalDrawer.pages;
        [["connections", "Connections", "Search, or app"],
         ["rules", "Rules", ""],
         ["profiles", "Profiles", "New profile name"],
         ["blocklists", "Blocklists", "Blocklist URL"]].forEach(function (spec) {
            main.pageStack.replace(pg[spec[0]].createObject(main));
            const title = main.pageStack.currentItem.title;
            if (spec[2] !== "") {
                const input = findBy(main.contentItem, "placeholderText", spec[2]);
                if (input === null) throw new Error("no input found on " + spec[0]);
                input.text = "TITLECANARY";
            }
            loader.item.sample();
            const snap = loader.item.previous;
            const hit = Object.keys(snap).filter(function (k) {
                return k.indexOf("ToolBarPageHeader") >= 0 && snap[k].text === title && snap[k].vis === "V";
            });
            if (hit.length === 0) throw new Error("title heading not traced on " + spec[0] + " (" + title + ")");
            console.warn("probe-title-ok " + spec[0] + " " + title);
        });
        console.warn("probe-done on");
    }
}
"#;

const PROBE_OFF_BODY: &str = r#"
        if (loader.item !== null) {
            throw new Error("the TextTrace instance exists with the flag off");
        }
        console.warn("probe-done off");
    }
}
"#;
