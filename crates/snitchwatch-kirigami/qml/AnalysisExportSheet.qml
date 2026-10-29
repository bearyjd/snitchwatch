// Explicit cache-only request, immutable plain-text preview, private local Save.
// Dialog deliberately avoids OverlaySheet's unconditional touch-drag close().
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Dialog {
    id: sheet
    objectName: "analysisExportSheet"
    required property var controller
    title: "AI-ready cache summary"
    modal: true
    closePolicy: controller.saving ? Controls.Popup.NoAutoClose
                                  : Controls.Popup.CloseOnEscape | Controls.Popup.CloseOnPressOutside
    width: Math.min(parent ? parent.width - 24 : 900, 900)
    height: Math.min(parent ? parent.height - 24 : 800, 800)
    anchors.centerIn: parent

    function changedFilters() {
        if (!controller.saving) controller.cancel();
    }
    function filtersJson() {
        let filters = {};
        if (windowEnabled.checked) {
            const from = parseUtc(fromUtc.text);
            const to = parseUtc(toUtc.text);
            if (from === null || to === null || from >= to)
                return null;
            filters.window = { fromMs: from, toMs: to };
        }
        if (appChoice.currentIndex === 1) filters.app = { kind: "path", value: exactPath.text };
        if (appChoice.currentIndex === 2) filters.app = { kind: "missingPath" };
        if (verdictEnabled.checked) {
            filters.verdicts = [];
            for (let i = 0; i < verdictRepeater.count; ++i) {
                const control = verdictRepeater.itemAt(i);
                if (control.checked) filters.verdicts.push(control.modelData);
            }
        }
        return JSON.stringify(filters);
    }
    function parseUtc(text) {
        const match = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,3}))?Z$/.exec(text);
        if (!match) return null;
        const ms = Date.parse(text);
        if (!Number.isFinite(ms)) return null;
        const canonical = match[1] + "." + ((match[2] || "") + "000").slice(0, 3) + "Z";
        return new Date(ms).toISOString() === canonical ? ms : null;
    }
    onClosed: {
        if (!controller.saving) controller.cancel();
    }
    Timer {
        // Always runs while the page owns this dialog, including an open preview
        // or admitted Save with no subsequent bridge messages.
        interval: 100
        running: true
        repeat: true
        onTriggered: sheet.controller.poll()
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing
        Controls.ScrollView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            contentWidth: availableWidth
            ColumnLayout {
                width: parent.width
                Controls.Label {
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    text: "This summary uses the bridge's shared recent cache and can include other users' observations. It is incomplete, not a record of network sessions or proof that traffic did not occur. Counts and timestamps are observations; byte totals, direction and durations are unavailable."
                }
                Controls.Label {
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    text: "Approved application basenames remain readable and may identify software. Other application and destination identities use pseudonyms that change for each export. Previewing creates no file. Save creates a private local JSON file that remains until you remove it. Nothing is uploaded."
                }
                Controls.Label {
                    objectName: "analysisExportScope"
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    text: sheet.controller.scopeText
                }
                Controls.CheckBox {
                    id: windowEnabled
                    objectName: "analysisExportWindowEnabled"
                    text: "Filter by an independent UTC window [from, to)"
                    enabled: !sheet.controller.saving
                    onToggled: sheet.changedFilters()
                }
                Controls.TextField {
                    id: fromUtc
                    objectName: "analysisExportFromUtc"
                    Layout.fillWidth: true
                    enabled: windowEnabled.checked && !sheet.controller.saving
                    placeholderText: "From UTC: 2026-10-24T00:00:00Z"
                    Accessible.name: "Export window start in UTC"
                    onTextEdited: sheet.changedFilters()
                }
                Controls.TextField {
                    id: toUtc
                    objectName: "analysisExportToUtc"
                    Layout.fillWidth: true
                    enabled: windowEnabled.checked && !sheet.controller.saving
                    placeholderText: "To UTC, exclusive: 2026-10-25T00:00:00Z"
                    Accessible.name: "Export window end in UTC, exclusive"
                    onTextEdited: sheet.changedFilters()
                }
                Controls.ComboBox {
                    id: appChoice
                    objectName: "analysisExportAppChoice"
                    Layout.fillWidth: true
                    enabled: !sheet.controller.saving
                    model: ["All application identities", "Exact process path", "Missing process path category"]
                    Accessible.name: "Export application: " + displayText
                    onActivated: sheet.changedFilters()
                }
                Controls.TextField {
                    id: exactPath
                    objectName: "analysisExportExactPath"
                    Layout.fillWidth: true
                    visible: appChoice.currentIndex === 1
                    enabled: !sheet.controller.saving
                    placeholderText: "Exact process path (spelling is preserved)"
                    Accessible.name: "Export exact process path"
                    onTextEdited: sheet.changedFilters()
                }
                Controls.CheckBox {
                    id: verdictEnabled
                    objectName: "analysisExportVerdictEnabled"
                    text: "Choose export verdicts (none checked selects none)"
                    enabled: !sheet.controller.saving
                    onToggled: sheet.changedFilters()
                }
                Flow {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    Repeater {
                        id: verdictRepeater
                        model: ["allow", "deny", "blocklist", "pending", "unknown"]
                        Controls.CheckBox {
                            required property string modelData
                            objectName: "analysisExportVerdict-" + modelData
                            text: modelData
                            Accessible.name: "Verdict " + modelData
                            checked: true
                            enabled: verdictEnabled.checked && !sheet.controller.saving
                            onToggled: sheet.changedFilters()
                        }
                    }
                }
                Controls.Label {
                    id: validation
                    objectName: "analysisExportValidation"
                    Layout.fillWidth: true
                    visible: text.length > 0
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                }
                Controls.Label {
                    objectName: "analysisExportStatus"
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    text: sheet.controller.statusText
                }
                Controls.TextArea {
                    objectName: "analysisExportPreview"
                    Layout.fillWidth: true
                    Layout.minimumHeight: 160
                    readOnly: true
                    textFormat: TextEdit.PlainText
                    wrapMode: TextEdit.WrapAnywhere
                    selectByMouse: true
                    text: sheet.controller.previewText
                    Accessible.name: "Exact JSON bytes to be saved"
                }
                Controls.Label {
                    objectName: "analysisExportSavedLocation"
                    Layout.fillWidth: true
                    visible: text.length > 0
                    textFormat: Text.PlainText
                    wrapMode: Text.WrapAnywhere
                    text: sheet.controller.savedLocation
                }
            }
        }
        RowLayout {
            Layout.fillWidth: true
            Controls.Button {
                objectName: "analysisExportRequest"
                text: sheet.controller.busy ? "Replace request" : "Request preview"
                enabled: sheet.controller.available && !sheet.controller.saving
                onClicked: {
                    if (sheet.controller.saving) return;
                    const filters = sheet.filtersJson();
                    validation.text = filters === null ? "Enter a valid UTC window with a start before its end; use the Z suffix." : "";
                    if (filters !== null) sheet.controller.requestExport(filters);
                }
            }
            GuardedButton {
                objectName: "analysisExportSave"
                text: sheet.controller.saving ? "Saving…" : "Save privately"
                enabled: sheet.controller.previewReady && sheet.controller.available && !sheet.controller.saving
                onClicked: sheet.controller.save()
            }
            Controls.Button {
                objectName: "analysisExportCancel"
                text: "Cancel preview"
                enabled: (sheet.controller.busy || sheet.controller.previewReady) && !sheet.controller.saving
                onClicked: sheet.controller.cancel()
            }
            Item { Layout.fillWidth: true }
            Controls.Button {
                objectName: "analysisExportClose"
                text: "Close"
                enabled: !sheet.controller.saving
                onClicked: { if (!sheet.controller.saving) sheet.close(); }
            }
        }
    }
}
