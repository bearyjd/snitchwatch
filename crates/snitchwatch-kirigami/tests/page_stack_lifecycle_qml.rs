//! Exercise the production StackNav with Kirigami's real PageRow. Page
//! creation must neither warn about visual ownership nor retain removed pages.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

#[test]
fn page_routes_attach_and_destroy_pages_without_scene_warnings() {
    init_headless_qt_env();
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));
    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, object, _url| {
            root_ok.store(!object.is_null(), Ordering::SeqCst);
        })
    });
    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(PROBE), &QUrl::from(PROBE_URL));
        }
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);
    assert!(root_ok.load(Ordering::SeqCst), "null root:\n{captured}");
    let errors: Vec<_> = captured
        .lines()
        .filter(|line| {
            line.contains(PROBE_URL)
                || line.contains("qrc:/qt/qml/com/snitchwatch/shell/")
                || line.contains("TypeError")
                || line.contains("not placed in the graphics scene")
                || line.contains("DelegateModel::cancel")
        })
        .collect();
    assert!(errors.is_empty(), "page lifecycle warnings:\n{captured}");
    assert!(captured.contains(DONE), "probe did not finish:\n{captured}");
}

const PROBE_URL: &str = "qrc:/page_stack_lifecycle_probe.qml";
const DONE: &str = "PAGE_STACK_LIFECYCLE_DONE";
const PROBE: &str = r#"
import QtQuick
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ApplicationWindow {
    id: probe
    width: 1000
    height: 700
    visible: true
    property int destroyed: 0
    property int ticks: 0
    property var shell: null
    property bool shellExercised: false
    StackNav { id: nav; stack: probe.pageStack }
    Component {
        id: firstPage
        Kirigami.Page {
            title: "Connections"
            property int token: 0
            Component.onDestruction: probe.destroyed++
        }
    }
    Component {
        id: otherPage
        Kirigami.Page {
            title: "Rules"
            Component.onDestruction: probe.destroyed++
        }
    }
    pageStack.initialPage: nav.createPage(firstPage)

    function expect(ok, message) {
        if (!ok) throw new Error(message);
    }
    Timer {
        interval: 50
        repeat: true
        running: true
        onTriggered: {
            try {
                if (probe.ticks++ === 0) {
                    const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml");
                    probe.expect(component.status === Component.Ready, component.errorString());
                    // No bridge runtime is started. Keep first-run pages out
                    // of the way while exercising the production drawer.
                    probe.shell = component.createObject(null, {onboardingShown: true, silentStepShown: true});
                    probe.expect(probe.shell !== null, "main.qml creation failed");
                    probe.expect(probe.pageStack.depth === 1, "initial depth");
                    probe.expect(probe.pageStack.currentItem.title === "Connections", "initial page");
                    for (let i = 0; i < 10; ++i) {
                        const replacement = nav.replace(otherPage);
                        probe.expect(replacement === probe.pageStack.currentItem, "replace return");
                        probe.expect(replacement.parent !== null, "replace visual attachment");
                        const pushed = nav.push(firstPage, {token: 42});
                        probe.expect(pushed === probe.pageStack.currentItem, "push return");
                        probe.expect(pushed.token === 42, "initial properties");
                        probe.expect(probe.pageStack.depth === 2, "push depth");
                        probe.pageStack.pop();
                        probe.expect(probe.pageStack.depth === 1, "pop depth");
                        gc();
                    }
                    nav.push(firstPage);
                    const replacement = nav.replaceMain(otherPage);
                    probe.expect(probe.pageStack.depth === 1, "replaceMain kept detail page");
                    probe.expect(replacement === probe.pageStack.currentItem, "replaceMain return");
                    let rejected = false;
                    try { nav.replaceMain(null); } catch (error) { rejected = true; }
                    probe.expect(rejected, "missing component accepted");
                    probe.expect(probe.pageStack.currentItem === replacement
                                 && probe.pageStack.depth === 1, "failed creation changed route");
                    gc();
                }
                if (probe.destroyed === 22) {
                    if (!probe.shellExercised) {
                        const shell = probe.shell;
                        probe.expect(shell.pageStack.depth === 1
                                     && shell.pageStack.currentItem.title === "Connections"
                                     && shell.pageStack.currentItem.parent !== null, "production initial route");
                        shell.globalDrawer.actions[2].triggered();
                        probe.expect(shell.pageStack.depth === 1
                                     && shell.pageStack.currentItem.title === "Rules", "production Rules route");
                        shell.globalDrawer.actions[0].triggered();
                        probe.expect(shell.pageStack.depth === 1
                                     && shell.pageStack.currentItem.title === "Connections", "production Connections route");
                        shell.destroy();
                        probe.shell = null;
                        probe.shellExercised = true;
                        return;
                    }
                    stop();
                    console.warn("PAGE_STACK_LIFECYCLE_DONE");
                    Qt.quit();
                } else if (probe.ticks > 100) {
                    throw new Error("removed pages retained: destroyed " + probe.destroyed + " of 22");
                }
            } catch (error) {
                stop();
                console.error("page lifecycle: " + error);
                if (probe.shell) probe.shell.destroy();
                Qt.callLater(Qt.quit);
            }
        }
    }
}
"#;
