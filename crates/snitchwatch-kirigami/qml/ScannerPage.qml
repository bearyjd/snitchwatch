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

    header: Kirigami.InlineMessage {
        anchors {
            left: parent.left
            right: parent.right
            margins: Kirigami.Units.smallSpacing
        }
        type: Kirigami.MessageType.Error
        visible: page.controller && page.controller.errorText.length > 0
        text: page.controller ? page.controller.errorText : ""
    }

    ColumnLayout {
        width: page.width
        spacing: Kirigami.Units.largeSpacing

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            text: "Runs a one-shot, on-demand privileged scan (rootkit signatures, " +
                  "kernel-parameter drift, loaded-module anomalies, Secure Boot/lockdown " +
                  "state) via a polkit prompt. No persistent privileged process is ever " +
                  "started."
        }

        RowLayout {
            Controls.Button {
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
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                text: "• " + modelData.path + (modelData.detail ? " — " + modelData.detail : "")
            }
        }
    }
}
