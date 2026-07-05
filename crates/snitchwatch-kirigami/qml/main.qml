// Snitchwatch — Kirigami shell entry point.
//
// Task 5 scaffold: a Kirigami.ApplicationWindow with a placeholder page.
// Phase 3b fills the pageStack with the Connections / Rules / Blocklists /
// Traffic pages task-by-task. App identity is bound from the Rust `AppInfo`
// QObject (no hardcoded strings in QML).
import QtQuick
import QtQuick.Window
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

    // Blocklists tab models (Task 9). Same lifetime-owned-at-window-scope
    // pattern as connectionsModel above; the live bridge feed attaches the
    // same way once that follow-up wiring lands.
    BlocklistsModel {
        id: blocklistsModel
    }
    BlocklistEntriesModel {
        id: blocklistEntriesModel
    }

    // Page components, swapped into pageStack by the drawer actions below.
    Component {
        id: connectionsPageComponent
        ConnectionsPage {
            model: connectionsModel
        }
    }
    Component {
        id: blocklistsPageComponent
        BlocklistsPage {
            model: blocklistsModel
            entriesModel: blocklistEntriesModel
        }
    }

    // Tracks the last-seen pending count so we only raise on a *new* pending
    // arrival, not on every count change (e.g. 2 -> 3 while already visible).
    property int lastPendingCount: 0

    // Task 7 requirement 1 — raise/focus over fullscreen.
    //
    // When a novel connection needs a decision, the window must come to the
    // front even over a fullscreen game (Bazzite is gaming-focused; a novel
    // connection often fires right as a game launches). We use Qt's native
    // window-activation calls rather than assuming the compositor surfaces the
    // window on its own — the exact "does the prompt actually appear over a
    // fullscreen game" claim the GUI decision doc flagged as untested.
    //
    // MANUAL VERIFICATION STILL REQUIRED (cannot be done in a headless CI/
    // sandbox — needs a live Plasma/Wayland session):
    //   1. Launch a borderless-fullscreen app (e.g. `gamescope -f -- <app>` or
    //      a fullscreen Qt test window).
    //   2. Drive a pending connection so pendingCount goes 0 -> >0.
    //   3. Confirm THIS window raises and gains keyboard focus over the
    //      fullscreen surface. On Wayland, raise()/requestActivate() are
    //      subject to the compositor's focus-stealing-prevention policy; if it
    //      does not surface, the fallback is the KDE notification with a
    //      "Review" action (Task 17/19) — which is why that path exists.
    // Record the pass/fail result on real hardware before shipping.
    Connections {
        target: connectionsModel
        function onPendingCountChanged() {
            const now = connectionsModel.pendingCount;
            if (now > root.lastPendingCount) {
                if (root.visibility === Window.Minimized || root.visibility === Window.Hidden)
                    root.showNormal();
                root.raise();
                root.requestActivate();
            }
            root.lastPendingCount = now;
        }
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
                onTriggered: root.pageStack.replace(connectionsPageComponent)
            },
            Kirigami.Action {
                text: "Rules"
                icon.name: "view-list-details"
            },
            Kirigami.Action {
                text: "Blocklists"
                icon.name: "edit-delete"
                onTriggered: root.pageStack.replace(blocklistsPageComponent)
            },
            Kirigami.Action {
                text: "Traffic"
                icon.name: "office-chart-line"
            }
        ]
    }

    pageStack.initialPage: connectionsPageComponent
}
