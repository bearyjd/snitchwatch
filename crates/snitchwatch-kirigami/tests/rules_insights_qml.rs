//! P2.6 Part 2: the Rules tab's badges and findings are honest.
//!
//! - a zero-count rule is "Unused" only with saved counts and 14 days since
//!   the latest of counting began, the rule was created and the bridge's last
//!   gap; unsaved counts, a shorter period or a young rule give "No hits since
//!   <time>" (from the start of that period); a `nolog` rule is "not counted";
//! - "Analyze rules" marks a rule another rule shadows, names that rule and
//!   links to it; the marks go away (and say so) when the rules change;
//! - more than 2,000 enabled rules are not analysed, and say so;
//! - every label is PlainText and none says anything was removed or changed.
//!
//! Same probe shape as `rules_hits_qml.rs`; the analysis runs on a worker
//! thread, so the probe continues from `analysisJsonChanged`. Run headless
//! with `QT_QPA_PLATFORM=offscreen`.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cxx_qt_lib::{QByteArray, QGuiApplication, QQmlApplicationEngine, QUrl};

#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

mod common;
use common::{capture_stderr, init_headless_qt_env};

const PROBE_URL: &str = "qrc:/rules_insights_probe.qml";
const RULES_PAGE: &str = include_str!("../qml/RulesPage.qml");

/// The page's title bar (where the button lives) isn't instantiated outside
/// an application window, so the probe can't click it; this pins what it does.
#[test]
fn the_analyze_button_asks_the_model_and_waits_while_it_runs() {
    let button = RULES_PAGE
        .split("objectName: \"analyzeButton\"")
        .nth(1)
        .and_then(|rest| rest.split("Controls.Button").next())
        .expect("RulesPage.qml has an Analyze rules button");
    assert!(button.contains("text: \"Analyze rules\""), "{button}");
    assert!(
        button.contains("onClicked: page.model.analyze()"),
        "{button}"
    );
    assert!(button.contains("state !== \"running\""), "{button}");
}

#[test]
fn badges_and_findings_are_shown_honestly() {
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
    property int stage: 0
    readonly property real day: 86400000
    readonly property real now: Date.now()

    function check(ok, what) {
        if (!ok) {
            probeWindow.failures.push(what);
        }
    }
    function created(daysAgo) {
        return Math.floor((probeWindow.now - daysAgo * probeWindow.day) / 1000);
    }
    function rule(name, action, host, ageDays, extra) {
        const r = { name: name, displayName: name, enabled: true, action: action,
                    duration: "always", description: "",
                    operator: { type: "simple", operand: "dest.host", data: host,
                                sensitive: false },
                    precedence: false, nolog: false, readOnlyReason: null,
                    created: probeWindow.created(ageDays) };
        for (const key in (extra || {})) {
            r[key] = extra[key];
        }
        return r;
    }
    function setRules(rules) {
        rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: rules }));
    }
    function counts(sinceDaysAgo, persistent, lossy, gapDaysAgo) {
        rulesModel.applyServerMessageJson(JSON.stringify({
            action: "ruleHits",
            sinceUnixMs: probeWindow.now - sinceDaysAgo * probeWindow.day,
            lossy: lossy,
            lastGapUnixMs: gapDaysAgo === null ? null
                           : probeWindow.now - gapDaysAgo * probeWindow.day,
            storage: persistent ? { persistent: true }
                                : { persistent: false, reason: "no state directory" },
            hits: []
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
    function row(index) {
        page.rulesList.forceLayout();
        return page.rulesList.itemAtIndex(index);
    }
    function text(index, name) {
        const label = probeWindow.findChild(probeWindow.row(index), name);
        return label && label.visible ? label.text : "";
    }
    function header(name) {
        const label = probeWindow.findChild(page.header, name);
        return label && label.visible ? label.text : "";
    }
    function plain(name, where) {
        const label = where === "header" ? probeWindow.findChild(page.header, name)
                                          : probeWindow.findChild(probeWindow.row(1), name);
        return !!label && label.textFormat === Text.PlainText;
    }

    RulesPage {
        id: page
        anchors.fill: parent
        model: RulesModel { id: rulesModel }
    }

    Connections {
        target: rulesModel
        function onAnalysisJsonChanged() {
            probeWindow.advance();
        }
    }

    function advance() {
        const info = JSON.parse(rulesModel.analysisJson);
        try {
            if (probeWindow.stage === 1 && info.state === "done") {
                probeWindow.stage = 2;
                probeWindow.afterAnalysis();
            } else if (probeWindow.stage === 3 && info.state === "tooMany") {
                probeWindow.stage = 4;
                probeWindow.afterTooMany(info);
            }
        } catch (e) {
            probeWindow.failures.push("exception: " + e);
            probeWindow.finish();
        }
    }

    function finish() {
        try {
            if (probeWindow.failures.length > 0) {
                throw new Error("insights probe: " + probeWindow.failures.join("; "));
            }
        } finally {
            Qt.quit();
        }
    }

    function badges() {
        const rules = [
            probeWindow.rule("100-deny", "deny", "example.com", 40),
            probeWindow.rule("200-allow", "allow", "example.com", 40),
            probeWindow.rule("300-solo", "allow", "solo.example", 40),
            probeWindow.rule("400-quiet", "allow", "quiet.example", 40, { nolog: true }),
            probeWindow.rule("500-new", "allow", "new.example", 3)
        ];
        probeWindow.setRules(rules);

        // Nothing from the bridge yet: no badge.
        probeWindow.check(probeWindow.text(2, "hitsLabel") === "",
                          "label before counts: '" + probeWindow.text(2, "hitsLabel") + "'");

        // Saved, complete, 30 days: unused where the rule is old enough.
        probeWindow.counts(30, true, false, null);
        const unused = "Unused: no hits counted in the last 14 days";
        probeWindow.check(probeWindow.text(0, "hitsLabel") === unused,
                          "row 0: '" + probeWindow.text(0, "hitsLabel") + "'");
        probeWindow.check(probeWindow.text(2, "hitsLabel") === unused,
                          "row 2: '" + probeWindow.text(2, "hitsLabel") + "'");
        probeWindow.check(probeWindow.text(3, "hitsLabel") === "Not counted: this rule doesn't log",
                          "nolog: '" + probeWindow.text(3, "hitsLabel") + "'");
        probeWindow.check(probeWindow.text(4, "hitsLabel").startsWith("No hits since "),
                          "young rule: '" + probeWindow.text(4, "hitsLabel") + "'");

        // Not saved: never "unused".
        probeWindow.counts(30, false, false, null);
        probeWindow.check(probeWindow.text(2, "hitsLabel").startsWith("No hits since "),
                          "unsaved: '" + probeWindow.text(2, "hitsLabel") + "'");
        // A shorter period than the window.
        probeWindow.counts(5, true, false, null);
        probeWindow.check(probeWindow.text(2, "hitsLabel").startsWith("No hits since "),
                          "short period: '" + probeWindow.text(2, "hitsLabel") + "'");
        // A gap inside the window: only the time after it counts, so the
        // rule has been counted for 2 days, not 14.
        probeWindow.counts(30, true, true, 2);
        probeWindow.check(probeWindow.text(2, "hitsLabel") ===
            "No hits since " + page.formatTime(probeWindow.now - 2 * probeWindow.day),
            "gap: '" + probeWindow.text(2, "hitsLabel") + "'");
        // An old gap (a restart weeks ago) leaves a long enough period.
        probeWindow.counts(60, true, true, 20);
        probeWindow.check(probeWindow.text(2, "hitsLabel") === unused,
                          "old gap: '" + probeWindow.text(2, "hitsLabel") + "'");
        // A gap of unknown time leaves no period to trust.
        probeWindow.counts(30, true, true, null);
        probeWindow.check(probeWindow.text(2, "hitsLabel") ===
            "No hits since " + page.formatTime(probeWindow.now - 30 * probeWindow.day)
            + "; some may have been missed",
            "unknown gap: '" + probeWindow.text(2, "hitsLabel") + "'");

        probeWindow.counts(30, true, false, null);
        probeWindow.check(probeWindow.plain("hitsLabel", "row"), "hits label is not PlainText");
    }

    function beforeAnalysis() {
        probeWindow.check(probeWindow.text(1, "shadowLabel") === "",
                          "a finding before anyone asked");
        probeWindow.check(probeWindow.header("analysisSummary") === "",
                          "summary before anyone asked");
        probeWindow.stage = 1;
        rulesModel.analyze();
    }

    function afterAnalysis() {
        // 200-allow can never decide: 100-deny covers it and stops the scan.
        const finding = probeWindow.text(1, "shadowLabel");
        probeWindow.check(finding === "Never applies: 100-deny decides these connections instead.",
                          "finding: '" + finding + "'");
        probeWindow.check(probeWindow.text(0, "shadowLabel") === "", "the deny was flagged");
        probeWindow.check(probeWindow.text(2, "shadowLabel") === "", "an unrelated rule flagged");
        const summary = probeWindow.header("analysisSummary");
        probeWindow.check(summary.startsWith("1 rule may never decide a connection")
                          && summary.indexOf("compare exactly") > 0,
                          "summary: '" + summary + "'");
        probeWindow.check(probeWindow.plain("shadowLabel", "row")
                          && probeWindow.plain("analysisSummary", "header"),
                          "a finding label is not PlainText");
        for (const t of [finding, summary]) {
            for (const word of ["removed", "deleted", "disabled", "changed by", "fixed"]) {
                probeWindow.check(t.toLowerCase().indexOf(word) < 0, "'" + t + "' says " + word);
            }
        }

        // The link opens the rule that decides.
        const show = probeWindow.findChild(probeWindow.row(1), "shadowShow");
        probeWindow.check(show !== null && show.visible, "no link to the covering rule");
        if (show) {
            show.clicked();
        }
        probeWindow.check(page.inspectName === "100-deny",
                          "the link opened '" + page.inspectName + "'");
        page.inspectorSheet.close();

        // The bridge re-sends the whole list after every rule command; the
        // same list again takes nothing back.
        probeWindow.setRules([
            probeWindow.rule("100-deny", "deny", "example.com", 40),
            probeWindow.rule("200-allow", "allow", "example.com", 40),
            probeWindow.rule("300-solo", "allow", "solo.example", 40),
            probeWindow.rule("400-quiet", "allow", "quiet.example", 40, { nolog: true }),
            probeWindow.rule("500-new", "allow", "new.example", 3)
        ]);
        probeWindow.check(probeWindow.text(1, "shadowLabel") ===
                          "Never applies: 100-deny decides these connections instead.",
                          "a finding went with an unchanged list: '"
                          + probeWindow.text(1, "shadowLabel") + "'");

        // A changed rule list makes the findings untrue: they go, and say so.
        probeWindow.setRules([probeWindow.rule("100-deny", "deny", "example.com", 40)]);
        probeWindow.check(probeWindow.text(0, "shadowLabel") === "", "a finding survived a change");
        probeWindow.check(probeWindow.header("analysisSummary") ===
                          "The rules changed after the analysis. Analyze again.",
                          "stale summary: '" + probeWindow.header("analysisSummary") + "'");
        // The same list again changes nothing it would have to take back.
        probeWindow.setRules([
            probeWindow.rule("100-deny", "deny", "example.com", 40),
            probeWindow.rule("200-allow", "allow", "example.com", 40)
        ]);

        // Too many enabled rules.
        const many = [];
        for (let i = 0; i < 2001; i++) {
            many.push(probeWindow.rule("r" + String(i).padStart(5, "0"), "allow",
                                       "h" + i + ".example", 40));
        }
        probeWindow.setRules(many);
        probeWindow.stage = 3;
        rulesModel.analyze();
    }

    function afterTooMany(info) {
        probeWindow.check(info.enabled === 2001 && info.limit === 2000,
                          "limits: " + JSON.stringify(info));
        probeWindow.check(probeWindow.header("analysisSummary") ===
            "Too many rules to analyze: 2001 are enabled and the limit is 2000.",
            "too many: '" + probeWindow.header("analysisSummary") + "'");
        probeWindow.finish();
    }

    Timer {
        interval: 150
        running: true
        repeat: false
        onTriggered: {
            try {
                probeWindow.badges();
                probeWindow.beforeAnalysis();
            } catch (e) {
                probeWindow.failures.push("exception: " + e);
                probeWindow.finish();
            }
        }
    }

    Timer {
        interval: 20000
        running: true
        repeat: false
        onTriggered: {
            probeWindow.failures.push("the analysis never finished (stage "
                                      + probeWindow.stage + ")");
            probeWindow.finish();
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
        "RulesPage insights probe failed to load (QML parse error)"
    );
    let bad_lines: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains(PROBE_URL))
        .collect();
    assert!(
        bad_lines.is_empty(),
        "RulesPage insights probe reported errors:\n{}",
        bad_lines.join("\n")
    );
}
