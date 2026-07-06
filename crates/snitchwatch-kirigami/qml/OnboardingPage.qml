// First-run onboarding wizard (Task 12).
//
// Maps from `crate::wizard`'s `DaemonState` (a near-verbatim port of
// `snitchwatch-tauri::wizard`) + implied `web/js/onboarding.js`. Design
// decision recorded here per the plan: a single state-driven Kirigami.Page
// (not a four-page StackView) — the four states share almost all of their
// layout, so one page with state-conditional content avoided duplicating it
// four times.
//
// All daemon-detection/action logic lives in Rust (`WizardController`,
// wrapping the Qt-free `wizard.rs` port); this page only displays state and
// wires up its actions.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.Page {
    id: page
    title: "Set up Snitchwatch"

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
            // bout of retrying (e.g. after "Start daemon" fails again later)
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
                case "unitMissing": return "edit-download";
                case "unitInactive": return "media-playback-start";
                case "unreachableRetrying": return "view-refresh";
                default: return "security-high";
                }
            }
        }

        Kirigami.Heading {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            level: 2
            wrapMode: Text.WordWrap
            text: {
                switch (page.controller ? page.controller.state : "") {
                case "unitMissing": return "Daemon not installed";
                case "unitInactive": return "Daemon not running";
                case "unreachableRetrying": return "Waiting for the daemon";
                default: return "Checking daemon status…";
                }
            }
        }

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            horizontalAlignment: Text.AlignHCenter
            text: page.controller ? page.controller.detail : ""
        }

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            horizontalAlignment: Text.AlignHCenter
            opacity: 0.75
            visible: page.controller && page.controller.state === "unitMissing"
            text: "See docs/packaging/rpm-ostree-layering.md to layer the OpenSnitch " +
                  "daemon onto a stock Bazzite/Fedora image (rpm-ostree install " +
                  "opensnitch), or use the bluebuild batteries-included image instead."
        }

        Controls.Label {
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

            Controls.Button {
                text: "Start daemon"
                icon.name: "media-playback-start"
                visible: page.controller && page.controller.state === "unitInactive"
                enabled: page.controller && !page.controller.busy
                onClicked: page.controller.startUnit()
            }
            Controls.Button {
                text: "Retry"
                icon.name: "view-refresh"
                enabled: page.controller && !page.controller.busy
                onClicked: page.controller.probe()
            }
            Controls.Button {
                text: "Continue anyway"
                flat: true
                onClicked: page.dismissed()
            }
        }
    }
}
