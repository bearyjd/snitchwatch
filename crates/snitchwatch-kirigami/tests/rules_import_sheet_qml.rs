//! The import preview sheet (`RulesImportSheet.qml`, roadmap P2.7) headless:
//! it renders each section from `rules::io::group`'s JSON (the real Rust
//! grouping and default ticks), its Apply count follows the ticks, and every
//! label showing file or daemon text is plain text. `RulesPage.qml`'s
//! Export button reaches the controller, and its outcome shows as plain
//! text under the title.
//!
//! Assertions are QML `throw`s, reported against the probe URL on stderr;
//! the Rust side fails on any such line (see `common::capture_stderr`).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};
use snitchwatch_bridge::rule_io::{ImportItem, ImportKind};
use snitchwatch_bridge::rule_policy::RuleProblem;

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rules_import_sheet_probe.qml";

fn item(name: &str, kind: ImportKind, action: &str) -> ImportItem {
    ImportItem {
        index: 0,
        name: name.into(),
        display_name: name.into(),
        kind,
        changed_fields: Vec::new(),
        problems: Vec::new(),
        weakens: false,
        applies_to_all_apps: false,
        precedence: false,
        persists: true,
        enabled: true,
        action: action.into(),
        duration: "always".into(),
        description: String::new(),
        nolog: false,
        conditions: vec!["process.path is /usr/bin/<b>curl</b>".into()],
    }
}

fn preview_json() -> String {
    let mut refused = item("", ImportKind::Refused, "");
    refused.display_name = "Rule 5 (its name can't be used)".into();
    refused.problems = vec![RuleProblem {
        path: "name".into(),
        reason: "<i>reserved</i>".into(),
    }];
    let items = vec![
        // Ticked: a deny.
        item("<b>evil</b>-deny", ImportKind::Add, "deny"),
        // Unticked: an allow for every app.
        ImportItem {
            applies_to_all_apps: true,
            persists: false,
            ..item("allow-all", ImportKind::Add, "allow")
        },
        // Unticked: loosens a rule.
        ImportItem {
            weakens: true,
            changed_fields: vec!["action".into()],
            ..item("swap", ImportKind::Replace, "allow")
        },
        item("same", ImportKind::Unchanged, "deny"),
        refused,
    ];
    serde_json::to_string(&snitchwatch_kirigami::rules::io::group(&items)).unwrap()
}

#[test]
fn the_sheet_renders_sections_counts_ticks_and_shows_plain_text() {
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
    width: 900
    height: 900

    function expect(condition, message) {
        if (!condition) {
            throw new Error(message);
        }
    }

    function all(item, out) {
        if (!item) return out;
        out.push(item);
        for (let i = 0; i < item.children.length; i++) {
            probeWindow.all(item.children[i], out);
        }
        return out;
    }

    function named(name) {
        return probeWindow.all(sheet.contentItem, []).filter(function (i) {
            return i.objectName === name;
        });
    }

    function one(name) {
        const found = probeWindow.named(name);
        probeWindow.expect(found.length === 1, name + ": found " + found.length);
        return found[0];
    }

    Item {
        anchors.fill: parent
        RulesImportSheet {
            id: sheet
            controller: RulesIoController { id: controller }
        }
    }

    RulesPage {
        id: page
        width: 600
        height: 400
        model: RulesModel {}
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                sheet.enter = null;
                sheet.exit = null;
                controller.previewJson = PREVIEW_JSON;
                sheet.load();
                sheet.open();

                // Sections.
                probeWindow.expect(probeWindow.one("importSectionAdd").text === "New rules (2)",
                    "add section: " + probeWindow.one("importSectionAdd").text);
                probeWindow.expect(probeWindow.one("importSectionReplace").visible, "replace section");
                probeWindow.expect(probeWindow.one("importSectionRefused").visible, "refused section");
                probeWindow.expect(!probeWindow.one("importSectionUnchanged").visible,
                    "unchanged starts collapsed");
                sheet.showUnchanged = true;
                probeWindow.expect(probeWindow.one("importSectionUnchanged").visible,
                    "unchanged expands");
                probeWindow.expect(probeWindow.named("importProblem").length === 1,
                    "the refusal reason is listed");

                // Default ticks come from Rust: only the deny is ticked.
                const ticks = probeWindow.named("importTick");
                probeWindow.expect(ticks.length === 3, "three tickable rows: " + ticks.length);
                const ticked = ticks.filter(function (t) { return t.checked; }).length;
                probeWindow.expect(ticked === 1, "default ticked: " + ticked);
                for (const tick of ticks) {
                    probeWindow.expect(tick.text === "", "a checkbox carries text");
                }
                const apply = probeWindow.one("importApply");
                probeWindow.expect(apply.text === "Apply 1 change", "count: " + apply.text);
                probeWindow.expect(probeWindow.named("importCaution")
                    .filter(function (c) { return c.visible; }).length === 2,
                    "both unticked rows say why");
                probeWindow.expect(probeWindow.named("importBadge")
                    .some(function (b) { return b.text === "Applies to all apps"; }),
                    "all-apps badge");

                // The count follows the ticks.
                sheet.setTicked("allow-all", true);
                probeWindow.expect(apply.text === "Apply 2 changes", "after tick: " + apply.text);
                probeWindow.expect(ticks.filter(function (t) { return t.checked; }).length === 2,
                    "the checkbox follows");
                sheet.setTicked("<b>evil</b>-deny", false);
                sheet.setTicked("allow-all", false);
                probeWindow.expect(apply.text === "Apply 0 changes" && !apply.enabled,
                    "nothing ticked: " + apply.text);

                // Applying without a preview from the service is refused,
                // and said so.
                sheet.setTicked("swap", true);
                sheet.applyTicked();
                probeWindow.expect(controller.statusText.indexOf("no import preview") >= 0,
                    "status: " + controller.statusText);

                // Outcomes show per rule.
                controller.resultsJson = JSON.stringify({ "<b>evil</b>-deny": "Applied" });
                probeWindow.expect(probeWindow.named("importResult")
                    .some(function (r) { return r.visible && r.text === "Applied"; }),
                    "outcome shown");

                // Every label showing file text is plain text.
                let checked = 0;
                for (const node of probeWindow.all(sheet.contentItem, [])) {
                    if (node.textFormat === undefined || typeof node.text !== "string") continue;
                    if (node.text.indexOf("<b>") < 0 && node.text.indexOf("<i>") < 0) continue;
                    checked += 1;
                    probeWindow.expect(node.textFormat === Text.PlainText,
                        "markup rendered as rich text: " + node.text);
                }
                probeWindow.expect(checked >= 3, "markup-bearing labels checked: " + checked);

                // The page: with no bridge, Export says so under the title.
                probeWindow.expect(!page.header.visible, "the page header starts hidden");
                page.rulesIo.requestExport();
                probeWindow.expect(page.rulesIo.statusText.indexOf("isn't connected") >= 0,
                    "export status: " + page.rulesIo.statusText);
                probeWindow.expect(!page.rulesIo.busy, "not waiting for an answer");
                let status = null;
                for (let i = 0; i < page.header.children.length; i++) {
                    if (page.header.children[i].objectName === "rulesIoStatus") {
                        status = page.header.children[i];
                    }
                }
                probeWindow.expect(status !== null && status.visible && page.header.visible,
                    "the outcome is shown under the title");
                probeWindow.expect(status.textFormat === Text.PlainText, "outcome is plain text");
            } finally {
                Qt.quit();
            }
        }
    }
}
"#
    .replace("PREVIEW_JSON", &serde_json::to_string(&preview_json()).unwrap());

    let guard = engine.as_mut().map(|engine| {
        let root_ok = root_ok.clone();
        engine.on_object_created(move |_engine, obj, _url| {
            // SAFETY: pointer only tested for null, never dereferenced.
            root_ok.store(!obj.is_null(), Ordering::SeqCst);
        })
    });

    let captured = capture_stderr(|| {
        if let Some(engine) = engine.as_mut() {
            engine.load_data(&QByteArray::from(qml.as_str()), &QUrl::from(PROBE_URL));
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
        "RulesImportSheet probe failed to load (QML parse error):\n{captured}"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL) || line.contains("RulesImportSheet.qml"))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RulesImportSheet probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
