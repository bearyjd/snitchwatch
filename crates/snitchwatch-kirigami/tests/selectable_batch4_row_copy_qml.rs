//! Selectable text, batch 4: the right-click Copy of the Recommended rules
//! rows (program rows, "No longer recommended" rows) and of a Profiles rule
//! row carries EVERY text the row shows, in the shown order, checked on the
//! real pages (not by reading their source): the copy is compared with the
//! visible Labels of the row, collapsed and in the "Remove…" confirming state.
//! Run headless with `QT_QPA_PLATFORM=offscreen QT_QUICK_CONTROLS_STYLE=Basic`.
#[allow(unused_imports)]
use snitchwatch_kirigami::bridge_bindings as _;
#[allow(unused_imports)]
use snitchwatch_kirigami::curated_defaults_model as _;

#[path = "../src/accessibility_test.rs"]
mod accessibility;
mod common;
#[path = "../src/key_repeat_test.rs"]
mod key_repeat;
#[path = "support/probe_runner.rs"]
mod probe_runner;

const PROBE_URL: &str = "qrc:/selectable_batch4_row_copy_probe.qml";
const DONE_MARKER: &str = "SELECTABLE_BATCH4_ROW_COPY_DONE";

const QML: &str = r#"
import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

Window {
    id: probe
    visible: true
    width: 1700
    height: 1300
    readonly property string doneMarker: "SELECTABLE_BATCH4_ROW_COPY_DONE"
    // @a11y-guard
    // @list-keys

    CuratedDefaultsModel { id: curated }
    ProfilesModel { id: profiles }
    Item {
        x: 0; y: 0; width: 850; height: 1300
        RecommendedRulesPage { id: rec; anchors.fill: parent; model: curated }
    }
    Item {
        x: 850; y: 0; width: 850; height: 1300
        ProfilesPage { id: prof; anchors.fill: parent; model: profiles }
    }

    property int ran: 0
    // send + six checks + the final count.
    readonly property int expectedSteps: 8

    function handlerOf(item) {
        for (let i = 0; i < item.data.length; ++i) if (item.data[i].objectName === "rowCopyMenu") return item.data[i];
        return null;
    }
    // The visible Labels under `root` (a copy-menu row keeps its texts as Labels),
    // in the order they are drawn.
    function shown(root) {
        const out = [];
        function visit(it) {
            if (it instanceof Controls.Label && it.visible && it.text.length > 0) {
                const p = it.mapToItem(root, 0, 0);
                out.push({ y: p.y, x: p.x, text: it.text });
            }
            const kids = it.children || [];
            for (let i = 0; i < kids.length; ++i) visit(kids[i]);
        }
        visit(root);
        out.sort((a, b) => a.y - b.y || a.x - b.x);
        return out.map(o => o.text);
    }
    function sameAsShown(row, h, what) {
        const want = probe.shown(row);
        probe.expect(want.length >= 3, what + ": too few visible texts " + JSON.stringify(want));
        probe.expect(h.copyText.length > 0 && h.copyText.indexOf("\n\n") < 0 && !h.copyText.endsWith("\n"),
                     what + ": blank lines or empty copy: " + JSON.stringify(h.copyText));
        probe.expect(h.copyText === want.join("\n"),
                     what + ": copy " + JSON.stringify(h.copyText) + " != shown " + JSON.stringify(want));
    }
    function entry(id, on, status) {
        return { id: id, program: "/usr/bin/" + id, allows: "ALLOWS " + id, why: "WHY " + id,
                 on: on, status: status };
    }
    function recRow(i) { return rec.entriesList.itemAtIndex(i); }
    function orphanRow(i) { return probe.find(rec.entriesList.footerItem, it => it.objectName === "orphanRows").itemAt(i); }
    function profList() { return probe.find(prof, it => it.objectName === "" && it.count !== undefined && it.itemAtIndex !== undefined); }
    function ready() {
        if (rec.entriesList.count !== 2) return "two entries";
        for (let i = 0; i < 2; ++i) if (!probe.stable("rec" + i, probe.recRow(i))) return "rec row " + i;
        const footer = rec.entriesList.footerItem;
        if (!probe.stable("footer", footer) || !footer.visible) return "the orphans section";
        const l = probe.profList();
        if (!l || l.count !== 2) return "two profiles";
        for (let i = 0; i < 2; ++i) if (!probe.stable("prof" + i, l.itemAtIndex(i))) return "profile row " + i;
        return "";
    }

    Component.onCompleted: {
        // A step limit (SW_STEPS) that cuts the run short would pass vacuously.
        probe.expect(probe.stepLimit >= probe.expectedSteps, "SW_STEPS cuts the probe short: " + probe.stepLimit);
        probe.step("send", null, function () {
            const nm = probe.entry("nm", true, "editedByYou");
            const flat = probe.entry("flatpak", false, "off");
            flat.problem = "PROBLEMTEXT";
            curated.applyServerMessageJson(JSON.stringify({ action: "setCuratedDefaults",
                entries: [flat, nm], storage: { persistent: true },
                orphans: [{ id: "gone", enabled: true, problem: "ORPHANPROBLEM" }] }));
            profiles.applyServerMessageJson(JSON.stringify({ action: "setProfiles",
                storage: { persistent: true }, appliesRules: true,
                profiles: [
                    { id: "home", name: "Home", networkMatchers: ["Home*", "Office"], active: true,
                      rules: [{ id: "r1", action: "deny", operand: "dest.host", data: "ads.example",
                                enforcement: "rule_installed" }] },
                    { id: "work", name: "Work", networkMatchers: [], active: false, rules: [] }] }));
        });
        probe.step("recommended entry row copies every shown text", probe.ready, function () {
            probe.ran++;
            const row = probe.recRow(0);
            const h = probe.handlerOf(row);
            probe.expect(h !== null, "the entry row has no RowCopyMenu");
            probe.expect(h.copyText.split("\n").indexOf("PROBLEMTEXT") >= 0, "problem missing: " + JSON.stringify(h.copyText));
            probe.sameAsShown(row, h, "entry row");
            probe.expect(h.copyText.split("\n")[0] === "/usr/bin/flatpak", "program is not first: " + JSON.stringify(h.copyText));
        });
        probe.step("entry row, no problem, then confirming", null, function () {
            probe.ran++;
            const row = probe.recRow(1);
            const h = probe.handlerOf(row);
            probe.sameAsShown(row, h, "plain entry row");
            probe.expect(h.copyText.indexOf("PROBLEMTEXT") < 0, "another row's problem leaked");
            probe.expect(h.copyText.indexOf("Remove the firewall's rule") < 0, "the question is copied before it is asked");
            probe.find(row, it => it.objectName === "removeButton").clicked();
        });
        probe.step("confirming adds the question", function () {
            const q = probe.find(probe.recRow(1), it => it.objectName === "removeQuestion" && it.visible);
            return q && probe.stable("recQ", q) ? "" : "the question";
        }, function () {
            probe.ran++;
            const row = probe.recRow(1);
            const h = probe.handlerOf(row);
            probe.expect(h.copyText.indexOf("Remove the firewall's rule") >= 0, "the question is not copied: " + JSON.stringify(h.copyText));
            probe.sameAsShown(row, h, "confirming entry row");
        });
        probe.step("orphan row, collapsed and confirming", null, function () {
            probe.ran++;
            const row = probe.orphanRow(0);
            const h = probe.handlerOf(row);
            probe.expect(h !== null, "the orphan row has no RowCopyMenu");
            probe.expect(h.copyText.split("\n").indexOf("ORPHANPROBLEM") >= 0, "orphan problem missing: " + JSON.stringify(h.copyText));
            probe.sameAsShown(row, h, "orphan row");
            probe.find(row, it => it.objectName === "orphanRemoveButton").clicked();
        });
        probe.step("orphan confirming adds the question", function () {
            const q = probe.find(probe.orphanRow(0), it => it.objectName === "orphanRemoveQuestion");
            return q && probe.stable("orphanQ", q) ? "" : "the orphan question";
        }, function () {
            probe.ran++;
            const row = probe.orphanRow(0);
            const h = probe.handlerOf(row);
            probe.sameAsShown(row, h, "confirming orphan row");
            probe.expect(h.copyText.indexOf("Remove the firewall's rule") >= 0, "orphan question not copied");
        });
        probe.step("profiles: row and rule row", null, function () {
            probe.ran++;
            const l = probe.profList();
            const home = probe.handlerOf(l.itemAtIndex(0)).copyText;
            probe.expect(home === "Home (active)\nHome*, Office", "home copy " + JSON.stringify(home));
            const work = probe.handlerOf(l.itemAtIndex(1)).copyText;
            probe.expect(work === "Work\nNo network matchers (manual activation only)", "work copy " + JSON.stringify(work));
            prof.inspectorSheet.enter = null;
            prof.inspectorSheet.exit = null;
            prof.openInspector({ profileId: "home", name: "Home", networkMatchers: "Home*, Office", isActive: true });
            const cond = probe.find(prof.inspectorSheet.contentItem, it => it.objectName === "profileRuleConditions" && it.visible);
            const act = probe.find(prof.inspectorSheet.contentItem, it => it.objectName === "profileRuleAction" && it.visible);
            probe.expect(cond !== null && act !== null, "no rule row");
            const h = probe.handlerOf(cond.parent);
            probe.expect(h !== null, "the rule row has no RowCopyMenu");
            probe.expect(act.text.length > 0 && cond.text.length > 0, "empty rule texts");
            probe.expect(h.copyText === act.text + " " + cond.text,
                         "rule copy " + JSON.stringify(h.copyText) + " != " + JSON.stringify(act.text + " " + cond.text));
        });
        // Last: every step above really ran (a skipped step must not pass).
        probe.step("steps executed == " + probe.expectedSteps, null, function () {
            probe.expect(probe.ran === probe.expectedSteps - 2,
                         "checks executed " + probe.ran + " != " + (probe.expectedSteps - 2));
        });
    }
}
"#;

#[test]
fn row_copy_text_has_every_visible_text() {
    probe_runner::run(QML, PROBE_URL, DONE_MARKER);
}
