//! About must be reachable from a healthy window, and must not keep showing a
//! daemon version once the daemon (or the bridge link) is gone. This creates
//! `main.qml` with no daemon problem (the banner is hidden), opens About
//! through the global drawer's action, then walks the version row through
//! daemon down / recovery with the bridge link up throughout, and through a
//! bridge-link loss.
//!
//! Limit, stated plainly: like `honest_ui_main_banner_qml.rs` this never runs
//! the event loop (under one `main.qml` starts the bridge and never quits), so
//! it proves the action's wiring (present, visible, enabled, named, opens the
//! page) but NOT real key delivery. The action is a plain `Kirigami.Action`
//! in the same drawer as every other page, so it gets that drawer's keyboard
//! handling. A probe error is a QML warning naming the probe URL. Run
//! headless with `QT_QPA_PLATFORM=offscreen`.
use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::about_model as _;
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/about_navigation_probe.qml";
const SHELL_QML_PREFIX: &str = "qrc:/qt/qml/com/snitchwatch/shell/qml/";

const PROBE: &str = r#"
import QtQuick

QtObject {
    function find(item, name) {
        if (!item) return null;
        if (item.objectName === name) return item;
        const kids = item.children || [];
        for (let i = 0; i < kids.length; i++) {
            const hit = find(kids[i], name);
            if (hit) return hit;
        }
        return null;
    }
    function check(cond, msg) { if (!cond) throw new Error(msg); }

    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml");
        if (component.status !== Component.Ready) throw new Error("main.qml: " + component.errorString());
        const main = component.createObject(null, {});
        check(main !== null, "main.qml was not created");

        // Healthy: no daemon-health banner, yet About is one drawer action away.
        const actions = main.globalDrawer.actions.filter(a => a.text === "About");
        check(actions.length === 1, "the drawer has " + actions.length + " About actions");
        check(actions[0].visible && actions[0].enabled, "the About action is not usable");
        actions[0].trigger();
        const page = main.pageStack.currentItem;
        check(find(page, "aboutCopyButton") !== null, "About did not open from the drawer");

        const report = (up) => main.daemonRefs.health.applyServerMessageJson(JSON.stringify({
            action: "diagnosticsReport",
            checks: [{ kind: "daemon_reachable", status: up ? { status: "ok" } : { status: "failed", detail: "down" } }]
        }));
        const stats = () => main.daemonRefs.stats.applyServerMessageJson(JSON.stringify({
            action: "daemonStatistics", daemonVersion: "1.8.0", uptime: 1, rules: 1,
            connections: 1, ignored: 0, accepted: 1, dropped: 0, ruleHits: 1, ruleMisses: 0
        }));
        const shown = () => find(page, "aboutValue_daemon_version").text;

        // Bridge link up throughout; only the daemon goes away and comes back.
        main.bridgeFeedRef.ok = true;
        report(true);
        stats();
        check(shown() === "1.8.0", "connected shows '" + shown() + "'");
        report(false);
        check(shown() === "not connected", "daemon down shows '" + shown() + "'");
        check(main.daemonRefs.stats.daemonVersion === "", "the stale version was kept");
        // A statistics callback queued before the drop but run after the down
        // report, and a repeated down report (no edge), must not revive it.
        stats();
        check(shown() === "not connected", "a late statistics callback revived '" + shown() + "'");
        check(page.about.reportText.indexOf("1.8.0") < 0, "the copy report carries the stale version");
        report(false);
        check(shown() === "not connected", "a repeated down report left '" + shown() + "'");
        report(true);
        check(shown() === "not connected", "recovery revived '" + shown() + "' without fresh statistics");
        stats();
        check(shown() === "1.8.0", "fresh statistics after recovery show '" + shown() + "'");
        check(page.about.reportText.indexOf("1.8.0") >= 0, "the copy report lost the fresh version");

        // Flapping: down, late statistics, up, down again.
        report(false);
        stats();
        check(shown() === "not connected", "flap 1 shows '" + shown() + "'");
        report(true);
        report(false);
        stats();
        check(shown() === "not connected", "flap 2 shows '" + shown() + "'");
        report(true);
        stats();
        check(shown() === "1.8.0", "after flapping, fresh statistics show '" + shown() + "'");

        // Losing the bridge link forgets it too.
        main.bridgeFeedRef.ok = false;
        check(shown() === "not connected", "after bridge loss shows '" + shown() + "'");
        main.bridgeFeedRef.ok = true;
        check(shown() === "not connected", "reconnect revived '" + shown() + "'");
    }
}
"#;

#[test]
fn about_opens_from_the_drawer_when_healthy_and_forgets_the_daemon_on_disconnect() {
    init_headless_qt_env();
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let captured = capture_stderr(|| {
        if let Some(mut engine) = engine.as_mut() {
            engine
                .as_mut()
                .load_data(&QByteArray::from(PROBE), &QUrl::from(PROBE_URL));
        }
    });
    let bad: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains(PROBE_URL) || l.contains(SHELL_QML_PREFIX))
        .collect();
    assert!(
        bad.is_empty(),
        "about navigation probe failed:\n{}",
        bad.join("\n")
    );
    let _ = app.as_mut();
}
