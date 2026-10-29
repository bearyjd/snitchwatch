// Security Scan page — Phase 6 report UI for Component B's privileged-tier
// scanner.
//
// Triggers an on-demand pkexec-gated deep scan (chkrootkit, kargs drift,
// module anomaly, Secure Boot/lockdown transition) and renders its report.
// All state/logic lives in `ScannerController` (Rust); this page only
// displays `reportJson` (parsed here via JSON.parse — see
// scanner_controller.rs's module doc for why this isn't a QAbstractListModel)
// and forwards the "run scan" action.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Security Scan"

    // Injected by main.qml so the controller's lifetime is owned there.
    property ScannerController controller

    // Emitted with the banner text each time a scan ends, alongside the
    // screen-reader announcement, so the announcement is observable in tests.
    signal outcomeAnnounced(string message)

    // Re-parsed whenever reportJson changes; {} before the first scan runs.
    readonly property var report: {
        if (!page.controller || page.controller.reportJson.length === 0) {
            return {};
        }
        try {
            return JSON.parse(page.controller.reportJson);
        } catch (e) {
            return {};
        }
    }

    // A new outcome always lands in view, whatever the scroll position.
    Connections {
        target: page.controller
        function onSummaryTextChanged() { page.flickable.contentY = 0; }
        function onBusyChanged() {
            page.flickable.contentY = 0;
            if (!page.controller.busy) page.announcePendingOutcome();
        }
    }

    // The summary banner changes to the new outcome in place (the previous
    // one stays up while a retry runs), which assistive tech does not report
    // on its own. The controller owns the
    // not-yet-announced outcome, so a scan that finished while this page was
    // not alive is announced once the new page is attached to a window (pages
    // are built parentless and attached afterwards, see StackNav), never from
    // a detached item, and never twice.
    function announcePendingOutcome() {
        // The one guard: while detached, hidden, or in a hidden window the
        // outcome stays pending and `onAttachedChanged` retries.
        if (!page.controller || !page.attached) return;
        const message = page.controller.takePendingOutcome();
        if (message.length === 0) return;
        summaryBanner.Accessible.announce(message, Accessible.Polite);
        page.outcomeAnnounced(message);
    }
    readonly property bool attached: visible && Window.window !== null && Window.window.visible
    onAttachedChanged: if (attached) page.announcePendingOutcome()

    // `errorText` includes the scanner's stderr and binary path, so it sits
    // in a PlainText label, never in the InlineMessage (issue #51).
    header: ColumnLayout {
        visible: page.controller && page.controller.errorText.length > 0
        spacing: Kirigami.Units.smallSpacing

        Kirigami.InlineMessage {
            objectName: "scannerStatusMessage"
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            visible: true
            type: Kirigami.MessageType.Error
            text: page.controller && page.controller.errorTitle.length > 0
                ? page.controller.errorTitle : "The deep scan failed."
        }
        Controls.Label {
            Layout.fillWidth: true
            Layout.leftMargin: Kirigami.Units.largeSpacing
            Layout.rightMargin: Kirigami.Units.largeSpacing
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            objectName: "scannerErrorText"
            text: page.controller ? page.controller.errorText : ""
        }
    }

    ColumnLayout {
        width: page.width
        spacing: Kirigami.Units.largeSpacing

        // Outcome of the last attempt, above the sections so it is never
        // below the fold. Counts only; error detail stays in the header.
        Kirigami.InlineMessage {
            id: summaryBanner
            objectName: "scannerSummary"
            Accessible.name: text
            Layout.fillWidth: true
            visible: page.controller && page.controller.summaryText.length > 0
            type: page.controller && page.controller.summaryKind === "error" ? Kirigami.MessageType.Error
                : page.controller && page.controller.summaryKind === "findings" ? Kirigami.MessageType.Warning
                : Kirigami.MessageType.Positive
            text: page.controller ? page.controller.summaryText : ""
        }
        // Shown alongside the previous outcome, which stays until this scan completes.
        Kirigami.InlineMessage {
            objectName: "scannerBusy"
            Accessible.name: text
            Layout.fillWidth: true
            visible: page.controller && page.controller.busy
            type: Kirigami.MessageType.Information
            text: "Scanning…"
        }

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            text: "Runs a one-shot, on-demand privileged scan (rootkit signatures, " +
                  "kernel-parameter drift, loaded-module anomalies, Secure Boot/lockdown " +
                  "state) via a polkit prompt. No persistent privileged process is ever " +
                  "started."
        }

        RowLayout {
            GuardedButton {
                objectName: "scannerRunButton"
                text: page.controller && page.controller.busy ? "Scanning…" : "Run Deep Scan"
                enabled: page.controller && !page.controller.busy
                onClicked: page.controller.runScan()
            }
            Controls.BusyIndicator {
                visible: page.controller && page.controller.busy
                running: visible
            }
        }

        Kirigami.Separator {
            Layout.fillWidth: true
        }

        ScannerReportSection {
            title: "New anomalies"
            entries: page.report.new || []
            emptyText: "No new anomalies."
        }
        ScannerReportSection {
            title: "Still outstanding"
            entries: page.report.still_outstanding || []
            emptyText: "Nothing still outstanding."
        }
        ScannerReportSection {
            title: "Resolved since last scan"
            entries: page.report.resolved || []
            emptyText: "Nothing resolved this scan."
        }
        ScannerReportSection {
            title: "Informational"
            entries: page.report.informational || []
            emptyText: "No informational findings."
        }
        ScannerReportSection {
            title: "Skipped / unavailable checks"
            entries: (page.report.skipped || []).map(function(s) {
                return { path: s.check, detail: s.reason };
            })
            emptyText: "No checks were skipped."
        }
    }

    // Small inline component: a labeled list of {path, detail} rows, or an
    // empty-state label. Kept local to this page (not a separate .qml file)
    // since it's only used here, five times, with no other consumer.
    component ScannerReportSection: ColumnLayout {
        id: section
        required property string title
        required property var entries
        required property string emptyText
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        Kirigami.Heading {
            level: 3
            text: section.title + " (" + section.entries.length + ")"
        }
        Controls.Label {
            visible: section.entries.length === 0
            text: section.emptyText
            opacity: 0.7
        }
        Repeater {
            model: section.entries
            delegate: Controls.Label {
                objectName: "scannerFindingPath"
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                // Host file paths: anyone can name a file to look like markup.
                textFormat: Text.PlainText
                text: "• " + modelData.path + (modelData.detail ? " — " + modelData.detail : "")
            }
        }
    }
}
