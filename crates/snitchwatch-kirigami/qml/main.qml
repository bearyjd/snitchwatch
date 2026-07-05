// Snitchwatch — Kirigami shell entry point.
//
// Task 5 scaffold: a Kirigami.ApplicationWindow with a placeholder page.
// Phase 3b fills the pageStack with the Connections / Rules / Blocklists /
// Traffic pages task-by-task. App identity is bound from the Rust `AppInfo`
// QObject (no hardcoded strings in QML).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ApplicationWindow {
    id: root
    title: appInfo.appName
    width: 900
    height: 640
    minimumWidth: 500
    minimumHeight: 400

    // Rust-owned application identity (Task 5 proof of the Rust->QML binding).
    AppInfo {
        id: appInfo
    }

    // Core-loop connection model (Task 6). Owned here so its lifetime spans the
    // window; pages bind to it. Live bridge feed attaches in a follow-up.
    ConnectionsModel {
        id: connectionsModel
    }

    globalDrawer: Kirigami.GlobalDrawer {
        title: appInfo.appName
        titleIcon: "security-high"
        isMenu: false
        actions: [
            Kirigami.Action {
                text: "Connections"
                icon.name: "network-connect"
                checked: true
            },
            Kirigami.Action {
                text: "Rules"
                icon.name: "view-list-details"
            },
            Kirigami.Action {
                text: "Blocklists"
                icon.name: "edit-delete"
            },
            Kirigami.Action {
                text: "Traffic"
                icon.name: "office-chart-line"
            }
        ]
    }

    pageStack.initialPage: ConnectionsPage {
        model: connectionsModel
    }
}
