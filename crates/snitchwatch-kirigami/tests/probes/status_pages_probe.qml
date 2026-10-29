import QtQuick
import QtQuick.Window
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtTest
import com.snitchwatch.shell

// Q01 slice 6: the status, settings and first-run pages by keyboard alone.
// One window; each step mounts a page under `holder`, waits until it holds
// still (#237), then Tabs through it with real key events and asserts the
// exact ordered list of accessible names.
Window {
    id: probe
    visible: true
    width: 900
    height: 1100
    property var failures: []
    property int step: 0
    property int stalls: 0
    readonly property int maxStalls: 200
    readonly property int repeat: Qt.GroupSwitchModifier
    property var page: null
    property var counts: ({})
    property var itineraries: ({})
    property bool dismissed: false
    property bool finished: false
    // @a11y-guard

    // Replaced by the Rust test: where the stubs live.
    readonly property string denyPath: "@@DENY_PATH@@"
    readonly property string systemctlLog: "@@SYSTEMCTL_LOG@@"

    Controls.Button { id: start; text: "Start here"; x: 0; y: 0 }
    Item { id: holder; x: 0; y: 40; width: probe.width; height: probe.height - 40 }
    TestCase { id: tc; name: "status"; when: false }

    ScannerController { id: scanner }
    SettingsController { id: settingsCtl }
    GeoModel { id: geoM }
    TrafficModel { id: traffic }
    DaemonHealthModel { id: health }
    DenyStatusModel { id: deny; statusPath: probe.denyPath; bootIdPath: probe.denyPath }
    WizardController { id: wizard }
    CuratedDefaultsModel { id: curated }
    Connections {
        target: scanner
        function onBusyChanged() { if (scanner.busy) probe.bump("scan"); }
    }
    Connections {
        target: curated
        function onCuratedChangeRequested(json) { probe.bump("curated"); }
    }
    QtObject {
        id: exporter
        property bool saving: false
        property bool available: true
        property bool busy: false
        property bool previewReady: true
        property string scopeText: "2 selected observations in 1 groups"
        property string statusText: "Preview ready"
        property string previewText: "{}"
        property string savedLocation: ""
        function poll() {}
        function cancel() { probe.bump("exportCancel"); }
        function requestExport(filters) { probe.bump("exportRequest"); }
        function save() { probe.bump("exportSave"); }
    }
    AnalysisExportSheet { id: sheet; controller: exporter }

    Component { id: healthC; DaemonHealthPage { model: health; denyStatus: deny } }
    Component { id: diagC; DiagnosticsPage { controller: settingsCtl; geoModel: geoM } }
    Component { id: scanC; ScannerPage { controller: scanner } }
    Component { id: geoC; GeoPage { model: geoM } }
    Component { id: trafficC; TrafficPage { model: traffic } }
    Component { id: onboardC; OnboardingPage { controller: wizard; onDismissed: probe.dismissed = true } }
    Component {
        id: silentC
        SilentModeChoice {
            settings: settingsCtl
            onReviewRecommendedRules: probe.bump("review")
            onChosen: probe.bump("chosen")
        }
    }
    Component { id: groupsC; CuratedGroupsStep { model: curated; onFinished: probe.bump("finished") } }

    function expect(ok, what) { if (!ok) probe.failures.push(what); }
    function bump(key) { probe.counts[key] = (probe.counts[key] || 0) + 1; }
    function n(key) { return probe.counts[key] || 0; }
    function focused() { return probe.activeFocusItem; }
    function nameOf(item) {
        if (!item) return "(nothing)";
        const name = String(item.Accessible.name || "").trim();
        return name !== "" ? name : "?" + probe.describe(item);
    }
    function cycle(limit) {
        const first = probe.focused();
        const seen = [];
        for (let i = 0; i < limit; ++i) {
            tc.keyClick(Qt.Key_Tab);
            seen.push(probe.nameOf(probe.focused()));
            if (probe.focused() === first) break;
        }
        return seen;
    }
    // Tab round a dialog (it has no start button): names until one repeats.
    function cycleUnique(limit) {
        const seen = [];
        for (let i = 0; i < limit; ++i) {
            tc.keyClick(Qt.Key_Tab);
            const name = probe.nameOf(probe.focused());
            if (seen.indexOf(name) >= 0) break;
            seen.push(name);
        }
        return seen;
    }
    function tabToHere(name) {
        for (let i = 0; i < 30; ++i) {
            if (probe.nameOf(probe.focused()) === name) return true;
            tc.keyClick(Qt.Key_Tab);
        }
        return false;
    }
    // Tab from the start button through the mounted page, back to the start.
    function itinerary() {
        start.forceActiveFocus();
        return probe.cycle(40);
    }
    function sameList(a, b) { return JSON.stringify(a) === JSON.stringify(b); }
    function expectItinerary(view, expected) {
        const got = probe.itinerary();
        probe.itineraries[view] = got;
        probe.expect(probe.sameList(got, expected),
                     view + ": Tab order is " + JSON.stringify(got) + ", expected " + JSON.stringify(expected));
    }
    function sweepNamed(where, exact) {
        probe.sweep(where);
        for (const name of exact)
            probe.expect(probe.names[name] === true, where + ": no control named '" + name + "'");
    }
    function mount(component) {
        if (probe.page) { probe.page.destroy(); probe.page = null; }
        // As StackNav does: built parentless, attached afterwards.
        probe.page = component.createObject(null, {});
        probe.page.parent = holder;
        probe.page.anchors.fill = holder;
    }
    function pageReady() {
        return probe.page && probe.stable("page", probe.page) ? "" : "the mounted page to hold still";
    }
    function byName(name) { return probe.find(probe.page, it => it.objectName === name); }
    function byAccName(name) {
        return probe.find(probe.page, it => it.Accessible && it.Accessible.name === name && it.activeFocusOnTab);
    }
    function tabTo(name, limit) {
        start.forceActiveFocus();
        for (let i = 0; i < (limit || 40); ++i) {
            tc.keyClick(Qt.Key_Tab);
            if (probe.nameOf(probe.focused()) === name) return true;
        }
        return false;
    }
    // Space as a user holds it: the control is pressed, the platform sends
    // repeats, the key is released.
    function holdSpace() {
        tc.keyPress(Qt.Key_Space);
        for (let i = 0; i < 2; ++i) {
            tc.keyRelease(Qt.Key_Space, probe.repeat);
            tc.keyPress(Qt.Key_Space, probe.repeat);
        }
    }
    // A key that began elsewhere, still down when the focus arrives.
    function heldFromBefore(key) {
        tc.keyPress(key, probe.repeat);
        tc.keyPress(key, probe.repeat);
        tc.keyRelease(key, probe.repeat);
        tc.keyRelease(key);
    }
    // Everything short of a fresh Space press changes nothing: Return,
    // Enter, Escape, and a Space (or Return) still held from before.
    function quietOn(what, snapshot) {
        const before = snapshot();
        for (const k of [Qt.Key_Return, Qt.Key_Enter]) {
            tc.keyClick(k);
            probe.expect(snapshot() === before, what + ": key " + k + " acted: " + snapshot());
        }
        probe.heldFromBefore(Qt.Key_Space);
        probe.expect(snapshot() === before, what + ": a held Space acted: " + snapshot());
        probe.heldFromBefore(Qt.Key_Return);
        probe.expect(snapshot() === before, what + ": a held Return acted: " + snapshot());
        probe.expect(probe.nameOf(probe.focused()) === what, what + ": focus left it for " + probe.nameOf(probe.focused()));
    }
    // Tab to `name`, check the quiet keys, then one press held and released
    // acts exactly once (`snapshot` shows it).
    function exercise(name, snapshot) {
        probe.expect(probe.tabTo(name), name + ": cannot Tab to it (at " + probe.nameOf(probe.focused()) + ")");
        probe.quietOn(name, snapshot);
        const before = snapshot();
        tc.keyPress(Qt.Key_Space);
        for (let i = 0; i < 2; ++i) {
            tc.keyRelease(Qt.Key_Space, probe.repeat);
            tc.keyPress(Qt.Key_Space, probe.repeat);
        }
        probe.expect(snapshot() === before, name + ": acted before the release: " + snapshot());
        tc.keyRelease(Qt.Key_Space);
    }
    function systemctlCalls() {
        const request = new XMLHttpRequest();
        try {
            request.open("GET", "file://" + probe.systemctlLog, false);
            request.send();
        } catch (e) { return 0; }
        const body = request.responseText || "";
        return body.length === 0 ? 0 : body.trim().split("\n").length;
    }
    function row(id, ip, action) {
        return { id: id, process: "firefox", processPath: null, dstHost: "github.com", dstIp: ip,
                 dstPort: 443, protocol: "tcp", direction: "outgoing", action: action,
                 bytesSent: 0, bytesReceived: 0, startedAtMs: 0 };
    }

    // Each step: a readiness gate (what it waits for, "" when ready) and an action.
    readonly property var steps: [
        // Daemon Health, with the deny section's copy buttons.
        { gate: () => "", run: () => { probe.requestActivate(); probe.mount(healthC); } },
        { gate: () => probe.pageReady(), run: () => {
            probe.expectItinerary("health", @@HEALTH_ITINERARY@@);
            probe.sweepNamed("health", ["Recheck"]);
        } },
        // Settings & Diagnostics: switches and selectors named "Label: value".
        { gate: () => "", run: () => { settingsCtl.refreshPreferences(); probe.mount(diagC); } },
        { gate: () => probe.pageReady(), run: () => {
            probe.expectItinerary("diagnostics", @@DIAGNOSTICS_ITINERARY@@);
            probe.sweepNamed("diagnostics", ["Launch at login", "Online research (RDAP) in decision dialog",
                "Allow remembers for: Forever", "New connections: Ask me (alerts)"]);
            const duration = probe.byName("defaultDurationBox");
            probe.expect(duration && duration.Accessible.name === "Allow remembers for: " + duration.displayText,
                         "the duration selector name is '" + (duration && duration.Accessible.name) + "'");
        } },
        // The two switches change what Snitchwatch does on its own: a fresh press only.
        { gate: () => !settingsCtl.busy ? "" : "settings idle", run: () => {
            const sw = probe.byAccName("Online research (RDAP) in decision dialog");
            sw.toggled.connect(() => probe.bump("rdap"));
            probe.exercise("Online research (RDAP) in decision dialog", () => String(probe.n("rdap")));
        } },
        { gate: () => (probe.n("rdap") >= 1 && !settingsCtl.busy) ? "" : "the RDAP switch", run: () => {
            probe.expect(probe.n("rdap") === 1, "the RDAP switch toggled " + probe.n("rdap") + " times");
            const sw = probe.byAccName("Launch at login");
            sw.toggled.connect(() => probe.bump("autostart"));
            probe.exercise("Launch at login", () => String(probe.n("autostart")));
        } },
        { gate: () => (probe.n("autostart") >= 1 && !settingsCtl.busy) ? "" : "the autostart switch", run: () => {
            probe.expect(probe.n("autostart") === 1, "Launch at login toggled " + probe.n("autostart") + " times");
        } },
        // Security Scan: Run Deep Scan starts a scan only on a fresh press.
        { gate: () => "", run: () => { probe.mount(scanC); } },
        { gate: () => probe.pageReady(), run: () => {
            probe.expectItinerary("scanner", ["Run Deep Scan", "Start here"]);
            probe.sweepNamed("scanner", ["Run Deep Scan"]);
            probe.exercise("Run Deep Scan", () => String(probe.n("scan")));
        } },
        { gate: () => (probe.n("scan") === 1 && !scanner.busy && scanner.summaryText !== "") ? "" : "the scan to end",
          run: () => {
            probe.expect(probe.n("scan") === 1, "the scan did not start exactly once: " + probe.n("scan"));
            probe.sweepNamed("scanner after a scan", ["Run Deep Scan"]);
            probe.expectItinerary("scanner after a scan", ["Run Deep Scan", "Start here"]);
        } },
        // Geography without a database (import and download buttons), then with rows.
        { gate: () => "", run: () => { geoM.dbAvailable = false; probe.mount(geoC); } },
        { gate: () => probe.pageReady(), run: () => {
            probe.expectItinerary("geography without a database", @@GEO_SETUP_ITINERARY@@);
            probe.sweepNamed("geography without a database", ["Import Country database…"]);
        } },
        { gate: () => "", run: () => {
            geoM.applyServerMessageJson(JSON.stringify({ action: "insertConnectionRows",
                rows: [probe.row("r1", "8.8.8.8", "allow"), probe.row("r2", "1.1.1.1", null)] }));
            geoM.dbAvailable = true;
            probe.mount(geoC);
        } },
        { gate: () => geoM.count > 0 ? probe.pageReady() : "geography rows", run: () => {
            probe.expectItinerary("geography rows", @@GEO_ROWS_ITINERARY@@);
            probe.sweepNamed("geography rows", []);
        } },
        // Traffic has nothing to operate; it must stay that way.
        { gate: () => "", run: () => { probe.mount(trafficC); } },
        { gate: () => probe.pageReady(), run: () => {
            probe.expectItinerary("traffic", ["Start here"]);
            probe.sweep("traffic");
        } },
        // Onboarding: Start daemon runs systemctl only on a fresh press.
        { gate: () => "", run: () => { probe.mount(onboardC); } },
        { gate: () => (probe.pageReady() === "" && !wizard.busy) ? "" : "the onboarding page and its probe",
          run: () => { wizard.state = "unitInactive"; } },
        { gate: () => probe.byAccName("Start daemon") ? probe.pageReady() : "Start daemon shown", run: () => {
            probe.expectItinerary("onboarding", ["Start daemon", "Retry", "Continue anyway", "Start here"]);
            probe.sweepNamed("onboarding", ["Start daemon", "Retry", "Continue anyway"]);
            probe.expect(probe.tabTo("Continue anyway"), "cannot Tab to Continue anyway");
            for (const k of [Qt.Key_Return, Qt.Key_Enter, Qt.Key_Escape]) tc.keyClick(k);
            probe.expect(!probe.dismissed, "Return, Enter or Escape dismissed onboarding");
            probe.exercise("Start daemon", () => wizard.busy + "|" + probe.systemctlCalls());
        } },
        { gate: () => probe.systemctlCalls() >= 1 ? "" : "the fake systemctl call", run: () => {
            probe.expect(probe.systemctlCalls() === 1, "Start daemon ran systemctl " + probe.systemctlCalls() + " times");
        } },
        // the silent-mode choice: nothing is saved until Continue.
        { gate: () => "", run: () => { probe.mount(silentC); } },
        { gate: () => probe.pageReady(), run: () => {
            probe.expectItinerary("silent mode", @@SILENT_ITINERARY@@);
            probe.sweepNamed("silent mode", ["Ask me (alerts)", "Allow and record", "Deny and record", "Continue"]);
            // Choosing a radio by keyboard only selects it.
            probe.expect(probe.tabTo("Deny and record"), "cannot Tab to Deny and record");
            probe.quietOn("Deny and record", () => settingsCtl.silentMode);
            probe.page.selected = "allow";
            tc.keyClick(Qt.Key_Space);
            probe.expect(probe.page.selected === "deny", "Space did not select Deny and record: " + probe.page.selected);
            probe.expect(settingsCtl.silentMode === "", "selecting a mode saved it: " + settingsCtl.silentMode);
        } },
        { gate: () => probe.page.denyWarningBox.visible ? probe.pageReady() : "the deny warning", run: () => {
            probe.expectItinerary("silent mode with the deny warning", @@SILENT_DENY_ITINERARY@@);
            probe.sweepNamed("silent mode with the deny warning", ["Review recommended background-service rules"]);
            probe.exercise("Review recommended background-service rules",
                           () => probe.n("review") + "|" + settingsCtl.silentMode);
        } },
        { gate: () => probe.n("review") >= 1 ? "" : "the review request", run: () => {
            probe.expect(probe.n("review") === 1, "Review fired " + probe.n("review") + " times");
        } },
        { gate: () => settingsCtl.silentMode === "deny" ? "" : "Deny saved", run: () => {
            // Back to Allow; Continue saves only on a fresh press.
            probe.page.selected = "allow";
            probe.exercise("Continue", () => probe.n("chosen") + "|" + (settingsCtl.silentMode === "allow"));
        } },
        { gate: () => probe.n("chosen") >= 1 ? "" : "Continue", run: () => {
            probe.expect(probe.n("chosen") === 1, "Continue fired " + probe.n("chosen") + " times");
            probe.expect(settingsCtl.silentMode === "allow", "Continue saved " + settingsCtl.silentMode);
        } },
        // Recommended rules groups: a switch asks only on a fresh press.
        { gate: () => "", run: () => {
            curated.applyServerMessageJson(JSON.stringify({ action: "authenticated",
                capabilities: ["curatedDefaults", "curatedGroupsV1", "curatedBroadOptInV1"] }));
            const groups = [
                { id: "system-services", title: "System services", description: "Clock and network." },
                { id: "app-updates", title: "App updates", description: "Flathub." }];
            const entry = (id, group) => ({ id: id, group: group, program: "/usr/bin/" + id,
                allows: id + " may connect to example.org on TCP port 443.", why: "Why " + id,
                on: false, status: "off" });
            curated.applyServerMessageJson(JSON.stringify({ action: "setCuratedDefaults",
                entries: [entry("chronyd", "system-services"), entry("flatpak", "app-updates")],
                groups: groups, storage: { persistent: true } }));
            probe.mount(groupsC);
        } },
        { gate: () => probe.page.groupsRepeater.count === 2 ? probe.pageReady() : "the groups", run: () => {
            probe.expectItinerary("recommended groups", @@GROUPS_ITINERARY@@);
            probe.sweepNamed("recommended groups", ["System services", "App updates", "Skip"]);
            probe.exercise("System services", () => String(probe.n("curated")));
        } },
        { gate: () => probe.n("curated") >= 1 ? "" : "the group request", run: () => {
            probe.expect(probe.n("curated") === 1, "the switch asked " + probe.n("curated") + " times");
            probe.expect(probe.tabTo("Skip"), "cannot Tab to Skip");
            for (const k of [Qt.Key_Return, Qt.Key_Enter]) tc.keyClick(k);
            probe.expect(probe.n("finished") === 0, "Return or Enter finished the step");
        } },
        // The two selectors that save a policy: a held key never walks through the choices.
        { gate: () => "", run: () => { probe.mount(diagC); } },
        { gate: () => (probe.pageReady() === "" && !settingsCtl.busy) ? "" : "the diagnostics page", run: () => {
            for (const objectName of ["silentModeBox", "defaultDurationBox"]) {
                const box = probe.byName(objectName);
                const name = box.Accessible.name;
                box.activated.connect(() => probe.bump(objectName));
                probe.expect(probe.tabTo(name), name + ": cannot Tab to it");
                const index = box.currentIndex;
                for (const key of [Qt.Key_Down, Qt.Key_Up, Qt.Key_Home, Qt.Key_End, Qt.Key_Space, Qt.Key_Return]) {
                    probe.heldFromBefore(key);
                    probe.expect(probe.n(objectName) === 0 && box.currentIndex === index && !box.popup.visible,
                                 name + ": a held key " + key + " acted (" + probe.n(objectName) + " choices, index "
                                 + box.currentIndex + ", popup " + box.popup.visible + ")");
                }
                // One arrow held down: it steps at most once.
                tc.keyPress(Qt.Key_Down);
                for (let i = 0; i < 3; ++i) {
                    tc.keyRelease(Qt.Key_Down, probe.repeat);
                    tc.keyPress(Qt.Key_Down, probe.repeat);
                }
                tc.keyRelease(Qt.Key_Down);
                probe.expect(probe.n(objectName) <= 1 && box.currentIndex <= index + 1,
                             name + ": a held Down chose " + probe.n(objectName) + " times, to index " + box.currentIndex);
            }
        } },
        // the export dialog, in a stub controller with a preview ready.
        { gate: () => "", run: () => { sheet.open(); } },
        { gate: () => (sheet.opened && probe.stable("sheet", sheet.contentItem)) ? "" : "the export dialog open", run: () => {
            probe.sweepNamed("export dialog", ["Request preview", "Save privately", "Cancel preview", "Close",
                "Export window start in UTC"]);
            const choice = probe.find(sheet.contentItem, it => it.objectName === "analysisExportAppChoice");
            probe.expect(choice.Accessible.name === "Export application: " + choice.displayText,
                         "the export selector name is '" + choice.Accessible.name + "'");
            const got = probe.cycleUnique(30);
            probe.itineraries.export = got;
            probe.expect(probe.sameList(got, @@EXPORT_ITINERARY@@),
                         "export: Tab order is " + JSON.stringify(got));
            // The filters, switched on by keyboard: more controls, all named.
            probe.expect(probe.tabToHere("Filter by an independent UTC window [from, to)"), "cannot Tab to the window filter");
            tc.keyClick(Qt.Key_Space);
            probe.expect(probe.tabToHere("Choose export verdicts (none checked selects none)"), "cannot Tab to the verdict filter");
            tc.keyClick(Qt.Key_Space);
            choice.currentIndex = 1;
        } },
        { gate: () => probe.stable("exactPath", probe.find(sheet.contentItem, it => it.objectName === "analysisExportExactPath"))
            ? "" : "the exact path field", run: () => {
            probe.sweepNamed("export dialog with every filter", ["Export window start in UTC",
                "Export window end in UTC, exclusive", "Export exact process path", "Verdict allow", "Verdict unknown"]);
            // The round starts where the focus is; read it from the dialog's first stop.
            const round = probe.cycleUnique(30);
            const at = round.indexOf("Filter by an independent UTC window [from, to)");
            const got = round.slice(at).concat(round.slice(0, at));
            probe.itineraries.exportFilters = got;
            probe.expect(probe.sameList(got, @@EXPORT_FILTERS_ITINERARY@@),
                         "export with every filter: Tab order is " + JSON.stringify(got));
            // Save is the one that writes a file.
            for (let i = 0; i < 30 && probe.nameOf(probe.focused()) !== "Save privately"; ++i) tc.keyClick(Qt.Key_Tab);
            probe.expect(probe.nameOf(probe.focused()) === "Save privately", "cannot Tab to Save privately");
            probe.quietOn("Save privately", () => String(probe.n("exportSave")));
            tc.keyPress(Qt.Key_Space);
            for (let i = 0; i < 2; ++i) {
                tc.keyRelease(Qt.Key_Space, probe.repeat);
                tc.keyPress(Qt.Key_Space, probe.repeat);
            }
            probe.expect(probe.n("exportSave") === 0, "Save privately acted before the release");
            tc.keyRelease(Qt.Key_Space);
            probe.expect(probe.n("exportSave") === 1, "Save privately did not act once: " + probe.n("exportSave"));
            tc.keyClick(Qt.Key_Escape);
        } },
        { gate: () => !sheet.visible ? "" : "the export dialog closed", run: () => {
            probe.expect(probe.n("exportSave") === 1, "closing the dialog saved");
            const bad = Object.keys(probe.missing);
            probe.expect(bad.length === 0, bad.length + " controls lack a meaningful Accessible.name: "
                         + bad.map(k => k + " (" + probe.missing[k] + ")").join("; "));
            probe.finished = true;
        } }
    ]

    Timer {
        interval: 100
        running: true
        repeat: true
        onTriggered: {
            if (probe.finished) return;
            try {
                if (probe.step < probe.steps.length) {
                    const current = probe.steps[probe.step];
                    const waiting = current.gate();
                    if (waiting !== "") {
                        if (++probe.stalls < probe.maxStalls) return;
                        throw new Error("step " + probe.step + " never ready for " + waiting);
                    }
                    probe.stalls = 0;
                    current.run();
                    probe.step++;
                }
            } catch (e) {
                probe.failures.push("step " + probe.step + " threw: " + e);
                probe.finished = true;
            }
            if (probe.finished) {
                stop();
                try {
                    if (probe.failures.length > 0)
                        throw new Error("status pages probe: " + probe.failures.join("; "));
                    console.warn("STATUS_PAGES_PROBE_DONE");
                } finally {
                    Qt.quit();
                }
            }
        }
    }
}
