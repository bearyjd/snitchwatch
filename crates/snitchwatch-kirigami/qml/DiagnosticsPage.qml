// Settings & Diagnostics page (Tasks 15, 16).
//
// Combines the autostart toggle (Task 15) and the crash-log viewer (Task 16)
// into one page — both are small, infrequently-touched shell-chrome surfaces
// with no shared state, so a single Kirigami.ScrollablePage keeps the drawer
// from growing two near-empty entries.
//
// All state/logic lives in `SettingsController` (Rust); this page only
// displays properties and forwards actions.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Settings & Diagnostics"

    // Injected by main.qml so the controller's lifetime is owned there.
    property SettingsController controller

    Component.onCompleted: {
        if (page.controller) {
            page.controller.refreshAutostart();
            page.controller.refreshCrashLog();
        }
    }

    ColumnLayout {
        width: page.width
        spacing: Kirigami.Units.largeSpacing

        Kirigami.FormLayout {
            Layout.fillWidth: true

            Controls.Switch {
                Kirigami.FormData.label: "Launch at login"
                checked: page.controller ? page.controller.autostartEnabled : false
                enabled: page.controller && !page.controller.busy
                onToggled: page.controller.setAutostart(checked)
            }

            Controls.Label {
                Kirigami.FormData.label: "Status"
                visible: page.controller && page.controller.autostartError.length > 0
                text: page.controller ? page.controller.autostartError : ""
                color: Kirigami.Theme.negativeTextColor
                wrapMode: Text.Wrap
            }
        }

        Kirigami.Separator {
            Layout.fillWidth: true
        }

        RowLayout {
            Layout.fillWidth: true

            Kirigami.Heading {
                Layout.fillWidth: true
                level: 3
                text: "Crash log"
            }

            Controls.BusyIndicator {
                running: page.controller && page.controller.busy
                visible: running
                implicitWidth: Kirigami.Units.iconSizes.small
                implicitHeight: Kirigami.Units.iconSizes.small
            }

            Controls.Button {
                text: "Refresh"
                icon.name: "view-refresh"
                enabled: page.controller && !page.controller.busy
                onClicked: page.controller.refreshCrashLog()
            }
        }

        Controls.TextArea {
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 20
            readOnly: true
            wrapMode: Controls.TextArea.NoWrap
            font.family: "monospace"
            selectByMouse: true
            text: page.controller ? page.controller.crashLogText : ""
        }
    }
}
