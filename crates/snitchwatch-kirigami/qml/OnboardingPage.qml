// First-run onboarding wizard (Task 12).
//
// A single state-driven Kirigami.Page: `connected` (page hidden) or
// `unreachableRetrying`. It only observes the authenticated bridge connection
// and cannot fix an unreachable bridge, so it offers no service control.
//
// All detection logic lives in Rust (`WizardController`, wrapping the Qt-free
// `wizard.rs`); this page only displays state and wires up Retry/Continue.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.Page {
    id: page
    title: "Set up Snitchwatch"
    property bool textTraceTitle: true

    // Injected by main.qml; owns detection state + actions.
    property WizardController controller

    // "Continue anyway" escape hatch (Task 12 requirement: onboarding must
    // never permanently block the main window). main.qml pops this page.
    signal dismissed()

    // Simple client-side retry cadence for UnreachableRetrying, with a capped
    // exponential backoff. This is onboarding UX only, not the safety-critical
    // AskRule countdown (Task 7), which stays server-side — see
    // PendingDecisionSheet.qml's docs for that distinction.
    property int retryDelayMs: 5000

    Timer {
        id: retryTimer
        interval: page.retryDelayMs
        repeat: false
        running: page.controller
            && page.controller.state === "unreachableRetrying"
            && !page.controller.busy
        onTriggered: {
            page.retryDelayMs = Math.min(page.retryDelayMs * 2, 30000);
            page.controller.probe();
        }
    }

    Connections {
        target: page.controller
        function onStateChanged() {
            // Reset the backoff once we leave the retrying state, so a fresh
            // bout of retrying (e.g. after the bridge drops again later)
            // starts from the short interval again.
            if (page.controller.state !== "unreachableRetrying") {
                page.retryDelayMs = 5000;
            }
        }
    }

    Component.onCompleted: {
        if (page.controller) {
            page.controller.probe();
        }
    }

    ColumnLayout {
        anchors.centerIn: parent
        width: Math.min(parent.width - Kirigami.Units.gridUnit * 4, Kirigami.Units.gridUnit * 26)
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Icon {
            Layout.alignment: Qt.AlignHCenter
            Layout.preferredWidth: Kirigami.Units.iconSizes.huge
            Layout.preferredHeight: Kirigami.Units.iconSizes.huge
            source: {
                switch (page.controller ? page.controller.state : "") {
                case "unreachableRetrying": return "view-refresh";
                default: return "security-high";
                }
            }
        }

        SelectableText {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            level: 2
            wrapMode: Text.WordWrap
            textFormat: Text.PlainText
            text: {
                switch (page.controller ? page.controller.state : "") {
                case "unreachableRetrying": return "Waiting for the bridge";
                default: return "Checking bridge connection…";
                }
            }
        }

        SelectableText {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            horizontalAlignment: Text.AlignHCenter
            // May carry the daemon probe's error message (issue #51).
            textFormat: Text.PlainText
            objectName: "wizardDetail"
            text: page.controller ? page.controller.detail : ""
        }

        SelectableText {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            horizontalAlignment: Text.AlignHCenter
            opacity: 0.75
            visible: page.controller && page.controller.state === "unreachableRetrying"
            text: "Setup and recovery steps: docs/packaging/system-bridge-integration.md " +
                  "(\"Missing socket, and console recovery\"). After connecting, Daemon " +
                  "Health shows the OpenSnitch daemon's status."
        }

        SelectableText {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            horizontalAlignment: Text.AlignHCenter
            opacity: 0.75
            visible: page.controller && page.controller.state === "unreachableRetrying"
            text: "Retrying automatically (next attempt in " + Math.round(page.retryDelayMs / 1000) + "s)."
        }

        Controls.BusyIndicator {
            Layout.alignment: Qt.AlignHCenter
            running: page.controller && page.controller.busy
            visible: running
        }

        RowLayout {
            Layout.alignment: Qt.AlignHCenter
            spacing: Kirigami.Units.smallSpacing

            RoundedButton {
                text: "Retry"
                icon.name: "view-refresh"
                enabled: page.controller && !page.controller.busy
                onClicked: page.controller.probe()
            }
            RoundedButton {
                text: "Continue anyway"
                flat: true
                onClicked: page.dismissed()
            }
        }
    }
}
