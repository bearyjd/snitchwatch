//! Selectable text, batch 6: the Rules page row's right-click Copy carries
//! EVERY text the row shows, in the order shown, checked on a real
//! `RulesPage` row (enabled / disabled, user / blocklist, with and without a
//! shadow finding, hit count, all-apps hint, read-only note) rather than by
//! reading the source. The expected lines are collected from the row's own
//! visible text items, so a text added to the row later but not to the copy
//! fails here. Run headless with
//! `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
mod common;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/selectable_batch6_row_copy_probe.qml";
const DONE_MARKER: &str = "SELECTABLE_BATCH6_ROW_COPY_DONE";

const QML: &str = r##"
import QtQuick
import QtQuick.Window
import QtQuick.Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1000
    height: 1000
    readonly property string doneMarker: "SELECTABLE_BATCH6_ROW_COPY_DONE"
    // @a11y-guard
    // @list-keys

    readonly property real day: 86400000
    readonly property real now: Date.now()
    property int ran: 0
    readonly property int expectedSteps: 4

    function rule(name, action, host, extra) {
        const r = { name: name, displayName: name, enabled: true, action: action,
                    duration: "always", description: "",
                    operator: { type: "simple", operand: "dest.host", data: host, sensitive: false },
                    precedence: false, nolog: false, readOnlyReason: null,
                    created: Math.floor((probe.now - 40 * probe.day) / 1000) };
        for (const key in (extra || {})) r[key] = extra[key];
        return r;
    }
    function rules(disableFirst) {
        return [
            probe.rule("100-deny", "deny", "example.com", { enabled: !disableFirst }),
            probe.rule("200-allow", "allow", "example.com"),
            probe.rule("snitchwatch-deny-github.com-443", "deny", "github.com",
                        { description: "snitchwatch interactive verdict" }),
            probe.rule("300-locked", "allow", "locked.example",
                        { readOnlyReason: "Snitchwatch can't edit this rule." }),
            probe.rule("400-off", "allow", "off.example", { enabled: false }),
            probe.rule("z00-blocklist:stevenblack:0001-doubleclick.net", "deny", "doubleclick.net")
        ];
    }

    RulesPage {
        id: page
        anchors.fill: parent
        model: RulesModel { id: rulesModel }
    }

    function rowNamed(name) {
        page.rulesList.forceLayout();
        for (let i = 0; i < page.rulesList.count; ++i) {
            const item = page.rulesList.itemAtIndex(i);
            if (item && item.name === name) return item;
        }
        return null;
    }
    function handlerOf(item) {
        for (let i = 0; i < item.data.length; ++i) if (item.data[i].objectName === "rowCopyMenu") return item.data[i];
        return null;
    }
    // Every visible text on the row, depth first = shown order. Buttons are
    // controls (Show rule, Delete), not text.
    function shownTexts(item, into) {
        if (!item || !item.visible) return into;
        if (item.checkable !== undefined) return into;
        if (typeof item.text === "string" && item.text.length > 0) into.push(item.text);
        const kids = item.children || [];
        for (let i = 0; i < kids.length; ++i) probe.shownTexts(kids[i], into);
        return into;
    }
    function copyLines(name) {
        const row = probe.rowNamed(name);
        probe.expect(row !== null, "no row " + name);
        probe.expect(probe.handlerOf(row) !== null, "row " + name + " has no RowCopyMenu");
        return probe.handlerOf(row).copyText.split("\n");
    }
    function shown(name) { return probe.shownTexts(probe.rowNamed(name).contentItem, []); }

    Component.onCompleted: {
        // A step limit (SW_STEPS) that cuts the run short would pass vacuously.
        probe.expect(probe.stepLimit >= probe.expectedSteps, "SW_STEPS cuts the probe short: " + probe.stepLimit);
        probe.step("setup: rules, hit counts, analysis", null, function () {
            probe.ran++;
            rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: probe.rules(false) }));
            rulesModel.applyServerMessageJson(JSON.stringify({ action: "ruleHits",
                sinceUnixMs: probe.now - 30 * probe.day, lossy: false, lastGapUnixMs: null,
                storage: { persistent: true }, hits: [] }));
            rulesModel.analyze();
        });
        probe.step("copy text is exactly the row's visible texts", function () {
            return rulesModel.analysisJson.length > 0 && JSON.parse(rulesModel.analysisJson).state === "done"
                ? "" : "analysis";
        }, function () {
            probe.ran++;
            let total = 0;
            for (const r of probe.rules(false)) {
                const want = probe.shown(r.name);
                const got = probe.copyLines(r.name);
                total += want.length;
                probe.expect(want.length >= 4, r.name + ": too few visible texts to mean anything: " + JSON.stringify(want));
                probe.expect(JSON.stringify(got) === JSON.stringify(want),
                             r.name + ": copy " + JSON.stringify(got) + " != shown " + JSON.stringify(want));
                for (const line of got) probe.expect(line.length > 0, r.name + ": empty line in the copy");
            }
            probe.expect(total >= 30, "fewer visible texts than the fixture should show: " + total);
            const shadowed = probe.copyLines("200-allow");
            probe.expect(shadowed.some(l => l.startsWith("Never decides: ")), "shadow finding missing: " + JSON.stringify(shadowed));
            const allApps = probe.copyLines("snitchwatch-deny-github.com-443");
            probe.expect(allApps.some(l => l.startsWith("Deleting this unblocks github.com")), "all-apps hint missing: " + JSON.stringify(allApps));
            probe.expect(allApps.indexOf("#2") >= 0 && allApps.length > 0, "precedence missing");
            probe.expect(probe.copyLines("300-locked").indexOf("read-only") >= 0, "read-only note missing");
            probe.expect(probe.copyLines("100-deny").some(l => l.startsWith("Unused: ")), "hit note missing");
            probe.expect(probe.copyLines("100-deny").indexOf("deny") >= 0, "action missing");
        });
        probe.step("disabled, blocklist and changed rows", null, function () {
            probe.ran++;
            probe.expect(probe.copyLines("100-deny")[0] === "●", "enabled glyph");
            probe.expect(probe.copyLines("400-off")[0] === "○", "disabled glyph");
            const bl = probe.copyLines("z00-blocklist:stevenblack:0001-doubleclick.net");
            probe.expect(bl.indexOf("blocklist: stevenblack") >= 0, "blocklist title missing: " + JSON.stringify(bl));
            probe.expect(bl.join("\n").indexOf("doubleclick.net") < 0,
                         "blocklist row shows an operator summary it must not copy: " + JSON.stringify(bl));
            // The same row after a change (first rule turned off) copies the new state.
            rulesModel.applyServerMessageJson(JSON.stringify({ action: "setRules", rules: probe.rules(true) }));
            const after = probe.copyLines("100-deny");
            probe.expect(after[0] === "○", "copy did not follow the rule turning off: " + JSON.stringify(after));
            probe.expect(JSON.stringify(after) === JSON.stringify(probe.shown("100-deny")),
                         "changed row: copy " + JSON.stringify(after) + " != shown " + JSON.stringify(probe.shown("100-deny")));
        });
        // Last: every step above really ran (a skipped step must not pass).
        probe.step("steps executed == " + probe.expectedSteps, null, function () {
            probe.expect(probe.ran + 1 === probe.expectedSteps,
                         "steps executed " + (probe.ran + 1) + " != " + probe.expectedSteps);
        });
    }
}
"##;

#[test]
fn rules_row_copy_text_has_every_visible_text() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
