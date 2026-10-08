//! P2.6 Part 1: the Rules tab's hit counts are honest.
//!
//! - nothing is shown until the bridge has sent counts, and not before
//!   counting has started;
//! - a rule that doesn't log is "not counted", never "0", and a rule that was
//!   counted without a hit says "No hits since <time>" (Part 2 adds "Unused");
//! - so is a rule whose name the bridge can't count (over 256 bytes);
//! - a count past a QML `int` is shown as it is, not capped;
//! - the header says since when the counts run and that they are approximate,
//!   says so when hits may be missing and since when, and says when they
//!   aren't saved across restarts (with the bridge's reason);
//! - every one of those labels is PlainText, so a rule name or a storage
//!   reason can't inject markup;
//! - a new count refreshes the rows without resetting the list.
//!
//! Same probe shape as `rules_all_apps_qml.rs`. Run headless with
//! `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rules_hits_probe.qml";

#[test]
fn hit_counts_are_shown_honestly() {
    init_headless_qt_env();

    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    let root_ok = Arc::new(AtomicBool::new(false));

    let qml = r#"
import QtQuick
import QtQuick.Window
import com.snitchwatch.shell

Window {
    id: probeWindow
    visible: true
    width: 1000
    height: 800

    property var failures: []
    property int resets: 0

    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }
    function setRules(rules) {
        rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: rules }));
    }
    function rule(name, nolog) {
        return { name: name, displayName: name, enabled: true, action: "allow",
                 duration: "always", description: "",
                 operator: { type: "simple", operand: "dest.host", data: "example.com",
                             sensitive: false },
                 precedence: false, nolog: nolog === true, readOnlyReason: null };
    }
    function hits(sinceMs, lossy, lastGapMs, persistent, reason, list) {
        rulesModel.applyServerMessageJson(JSON.stringify({
            action: "ruleHits",
            sinceUnixMs: sinceMs,
            lossy: lossy,
            lastGapUnixMs: lastGapMs,
            storage: persistent ? { persistent: true }
                                : { persistent: false, reason: reason },
            hits: list
        }));
    }
    function findChild(item, name) {
        if (!item) {
            return null;
        }
        if (item.objectName === name) {
            return item;
        }
        const kids = item.children || [];
        for (let i = 0; i < kids.length; i++) {
            const found = probeWindow.findChild(kids[i], name);
            if (found) {
                return found;
            }
        }
        return item.contentItem ? probeWindow.findChild(item.contentItem, name) : null;
    }
    function rowLabel(index) {
        page.rulesList.forceLayout();
        const row = page.rulesList.itemAtIndex(index);
        return probeWindow.findChild(row, "hitsLabel");
    }
    function rowText(index) {
        const label = probeWindow.rowLabel(index);
        return label && label.visible ? label.text : "";
    }
    function headerLabel(name) {
        const label = probeWindow.findChild(page.header, name);
        return label && label.visible ? label.text : "";
    }

    RulesPage {
        id: page
        anchors.fill: parent
        model: RulesModel { id: rulesModel }
    }

    Connections {
        target: rulesModel
        function onModelReset() {
            probeWindow.resets++;
        }
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                const T = 1800000000000;
                probeWindow.setRules([
                    probeWindow.rule("a"),
                    probeWindow.rule("b"),
                    probeWindow.rule("quiet", true),
                    probeWindow.rule("once"),
                    probeWindow.rule("big"),
                    probeWindow.rule("x".repeat(257))
                ]);

                // Nothing yet: no label on any row, no header.
                for (let i = 0; i < 6; i++) {
                    probeWindow.check(probeWindow.rowText(i) === "",
                                      "row " + i + " shows '" + probeWindow.rowText(i)
                                      + "' before any counts");
                }
                probeWindow.check(!page.header.visible, "header shown before any counts");

                // The bridge is there but hasn't seen the daemon's statistics.
                probeWindow.hits(null, false, null, true, "", []);
                probeWindow.check(probeWindow.rowText(0) === "",
                                  "a count before counting started: " + probeWindow.rowText(0));
                probeWindow.check(probeWindow.headerLabel("hitsSummary") ===
                    "Hit counts start when the firewall first reports statistics.",
                    "waiting text: " + probeWindow.headerLabel("hitsSummary"));

                // Counting, complete as far as anyone knows, saved.
                probeWindow.hits(T, false, null, true, "", [
                    { name: "a", count: 3, lastHitUnixMs: T + 5000 },
                    { name: "quiet", count: 9, lastHitUnixMs: T + 6000 },
                    { name: "b", count: 1, lastHitUnixMs: T + 7000 },
                    { name: "big", count: 3000000000, lastHitUnixMs: T + 9000 }
                ]);
                const a = probeWindow.rowText(0);
                probeWindow.check(a.startsWith("3 hits, last "), "row a: '" + a + "'");
                probeWindow.check(probeWindow.rowText(1).startsWith("1 hit, last "),
                                  "row b: '" + probeWindow.rowText(1) + "'");
                probeWindow.check(probeWindow.rowText(2) === "Not counted: this rule doesn't log",
                                  "nolog row: '" + probeWindow.rowText(2) + "'");
                // A rule of unknown age is never "Unused"; counted from when
                // counting began (rules_insights_qml.rs covers the badges).
                probeWindow.check(probeWindow.rowText(3).startsWith("No hits since "),
                                  "zero row: '" + probeWindow.rowText(3) + "'");
                probeWindow.check(probeWindow.rowText(4).startsWith("3000000000 hits, last "),
                                  "big row: '" + probeWindow.rowText(4) + "'");
                probeWindow.check(probeWindow.rowText(5) ===
                    "Not counted: this rule's name is too long or has control characters",
                    "long-name row: '" + probeWindow.rowText(5) + "'");
                const summary = probeWindow.headerLabel("hitsSummary");
                probeWindow.check(summary.startsWith("Hits counted by Snitchwatch since ")
                                  && summary.endsWith("; approximate.")
                                  && summary.indexOf("missing") < 0,
                                  "summary: '" + summary + "'");
                probeWindow.check(probeWindow.headerLabel("hitsStorage") === "",
                                  "storage shown while saved");

                // A refresh doesn't reset the list.
                const resetsBefore = probeWindow.resets;
                page.rulesList.currentIndex = 1;
                probeWindow.hits(T, false, null, true, "", [
                    { name: "a", count: 4, lastHitUnixMs: T + 8000 }
                ]);
                probeWindow.check(probeWindow.resets === resetsBefore,
                                  "the model was reset by a count update");
                probeWindow.check(page.rulesList.currentIndex === 1,
                                  "the selection moved: " + page.rulesList.currentIndex);
                probeWindow.check(probeWindow.rowText(0).startsWith("4 hits, last "),
                                  "refreshed row: '" + probeWindow.rowText(0) + "'");

                // Hits may be missing, and nothing is saved. The reason is
                // the bridge's text and must not be read as markup.
                probeWindow.hits(T, true, T + 60000, false,
                                 "state directory <b>/x</b>: gone", []);
                const lossy = probeWindow.headerLabel("hitsSummary");
                probeWindow.check(lossy.indexOf("approximate.") > 0
                                  && lossy.indexOf(" Hits may be missing before ") > 0
                                  && lossy.indexOf("No gap noticed since") < 0,
                                  "lossy summary: '" + lossy + "'");
                // A gap long past is still dated, and says nothing has been
                // noticed since. One of unknown time is not dated.
                probeWindow.hits(T, true, Date.now() - 30 * 86400000, true, "", []);
                const old = probeWindow.headerLabel("hitsSummary");
                probeWindow.check(old.indexOf(" Hits may be missing before ") > 0
                                  && old.endsWith(" No gap noticed since."),
                                  "old gap summary: '" + old + "'");
                probeWindow.hits(T, true, null, true, "", []);
                const undated = probeWindow.headerLabel("hitsSummary");
                probeWindow.check(undated.endsWith(" Some hits may be missing.")
                                  && undated.indexOf("before") < 0,
                                  "undated gap summary: '" + undated + "'");
                probeWindow.hits(T, true, T + 60000, false,
                                 "state directory <b>/x</b>: gone", []);
                probeWindow.check(probeWindow.headerLabel("hitsStorage") ===
                    "Hit counts are not saved across restarts: state directory <b>/x</b>: gone",
                    "storage text: '" + probeWindow.headerLabel("hitsStorage") + "'");
                for (const name of ["hitsSummary", "hitsStorage"]) {
                    const label = probeWindow.findChild(page.header, name);
                    probeWindow.check(label && label.textFormat === Text.PlainText,
                                      name + " is not PlainText");
                }
                const rowLabel = probeWindow.rowLabel(2);
                probeWindow.check(rowLabel && rowLabel.textFormat === Text.PlainText,
                                  "row label is not PlainText");

                // No reason: still says they aren't saved.
                probeWindow.hits(T, false, null, false, "", []);
                probeWindow.check(probeWindow.headerLabel("hitsStorage") ===
                    "Hit counts are not saved across restarts.",
                    "storage text without a reason: '"
                    + probeWindow.headerLabel("hitsStorage") + "'");

                if (probeWindow.failures.length > 0) {
                    throw new Error("hits probe: " + probeWindow.failures.join("; "));
                }
            } finally {
                Qt.quit();
            }
        }
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

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml), &QUrl::from(PROBE_URL));
        }
        if root_ok.load(Ordering::SeqCst) {
            if let Some(app) = app.as_mut() {
                app.exec();
            }
        }
    });
    drop(guard);

    assert!(
        root_ok.load(Ordering::SeqCst),
        "RulesPage hits probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RulesPage hits probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
