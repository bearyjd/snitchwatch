import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Window {
    id: root
    visible: true
    width: 1000; height: 900
    property string expectedArtifact: "EXPECTED_ARTIFACT"
    property string exactSelector: "EXACT_SELECTOR"
    property int phase: 0
    AnalysisExportController { id: controller }
    BridgeFeed { id: feed }
    AnalysisExportSheet { id: sheet; controller: controller }
    function check(condition, message) { if (!condition) throw new Error(message); }
    function find(name, item) {
        if (!item) item = sheet.contentItem;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; ++i) {
            const found = find(name, item.children[i]);
            if (found) return found;
        }
        return null;
    }
    Component.onCompleted: {
        try {
            controller.startBridgeFeed();
            controller.requestExport("{}");
            check(!controller.available && !controller.busy && !controller.previewReady && !controller.save(), "older bridge became empty success");
            check(controller.statusText.length > 0 && controller.previewText === "" && controller.savedLocation === "", "older bridge has no explanation");
            feed.sendClientJson('{"action":"requestCacheSummaryExport","schemaVersion":1,"requestId":"unsafe","filters":{"app":{"kind":"PAYLOAD-CANARY-DO-NOT-LOG"}}}');
            sheet.open();
        } catch (error) { console.warn("Error: " + error.message); Qt.quit(); }
    }
    Timer {
        interval: 10; running: true; repeat: true
        onTriggered: {
            try {
                if (root.phase === 0 && controller.available) {
                    find("analysisExportWindowEnabled").checked = true;
                    find("analysisExportFromUtc").text = "2026-02-30T01:00:00Z";
                    find("analysisExportToUtc").text = "2026-10-24T02:00:00Z";
                    check(sheet.filtersJson() === null, "nonexistent UTC date was silently normalized");
                    find("analysisExportFromUtc").text = "2026-10-24T01:00:00Z";
                    find("analysisExportToUtc").text = "2026-10-24T02:00:00Z";
                    find("analysisExportAppChoice").currentIndex = 1;
                    find("analysisExportExactPath").text = root.exactSelector;
                    find("analysisExportVerdictEnabled").checked = true;
                    for (const name of ["allow", "blocklist", "pending", "unknown"])
                        find("analysisExportVerdict-" + name).checked = false;
                    const filters = JSON.parse(sheet.filtersJson());
                    check(filters.app.value === root.exactSelector && filters.verdicts.length === 1 && filters.verdicts[0] === "deny", "exact filters changed");
                    find("analysisExportRequest").clicked();
                    root.phase = 1;
                } else if (root.phase === 1 && controller.previewReady) {
                    check(controller.previewText === root.expectedArtifact, "preview bytes changed");
                    check(find("analysisExportPreview").text === root.expectedArtifact && find("analysisExportPreview").textFormat === TextEdit.PlainText, "preview can render markup or rebuilds JSON");
                    check(find("analysisExportScope").text.includes("2 selected observations") && find("analysisExportScope").text.includes("1 groups"), "preview counts missing");
                    check(find("analysisExportScope").text.includes("Search, grouping and map filters do not apply"), "export scope is unclear");
                    check(controller.savedLocation === "", "preview created a saved location");
                    find("analysisExportCancel").clicked();
                    check(!controller.previewReady && controller.previewText === "", "cancel retained artifact");
                    find("analysisExportAppChoice").currentIndex = 2;
                    find("analysisExportVerdict-deny").checked = false;
                    const filters = JSON.parse(sheet.filtersJson());
                    check(filters.app.kind === "missingPath" && filters.verdicts.length === 0, "missing or empty selection widened");
                    find("analysisExportRequest").clicked();
                    root.phase = 2;
                } else if (root.phase === 2 && controller.previewReady) {
                    check(controller.previewText === root.expectedArtifact, "second preview changed bytes");
                    check(find("analysisExportSave").enabled, "valid artifact cannot save");
                    find("analysisExportSave").clicked();
                    root.phase = 3;
                } else if (root.phase === 3 && !controller.saving && controller.savedLocation !== "") {
                    check(find("analysisExportSavedLocation").text === controller.savedLocation && find("analysisExportSavedLocation").textFormat === Text.PlainText, "saved location was not shown locally as plain text");
                    console.warn("ANALYSIS_EXPORT_PROBE_COMPLETE");
                    Qt.quit();
                }
            } catch (error) { console.warn("Error: " + error.message); Qt.quit(); }
        }
    }
    Timer { interval: 15000; running: true; onTriggered: { console.warn("ANALYSIS_EXPORT_TIMEOUT"); Qt.quit(); } }
}
