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

    // Live-wiring hub (Task 13). Owns the app-level bridge status surface and is
    // the single inbound sink the models' request signals feed. The in-process
    // bridge itself is started in `main.rs` before this QML loads; `refresh()`
    // (below) just reflects its outcome.
    BridgeFeed {
        id: bridgeFeed
    }

    // Core-loop connection model (Task 6). Owned here so its lifetime spans the
    // window; pages bind to it. Its live outbound feed is started in the
    // window's Component.onCompleted via startBridgeFeed().
    ConnectionsModel {
        id: connectionsModel
    }

    // Blocklists tab models (Task 9). Outbound feeds started alongside the
    // others below; the subscribe/unsubscribe request signal is routed to the
    // bridge inbound via the Connections block further down.
    BlocklistsModel {
        id: blocklistsModel
    }
    BlocklistEntriesModel {
        id: blocklistEntriesModel
    }

    // Rules tab model (Task 10). Outbound feed started below; rule-change
    // request signal routed to the bridge inbound via the Connections block.
    RulesModel {
        id: rulesModel
    }

    // Traffic tab model (Task 11). Outbound feed started alongside the
    // others below; read-only (no request signal to route back).
    TrafficModel {
        id: trafficModel
    }

    // Task 13 live wiring. The bridge is already started (main.rs); here we (a)
    // reflect its status into the banner, and (b) start each model's outbound
    // feed task. startBridgeFeed()/refresh() are no-ops when the bridge failed
    // to start, so a degraded bridge still yields a working (if empty) window.
    Component.onCompleted: {
        bridgeFeed.refresh();
        connectionsModel.startBridgeFeed();
        blocklistsModel.startBridgeFeed();
        blocklistEntriesModel.startBridgeFeed();
        rulesModel.startBridgeFeed();
        trafficModel.startBridgeFeed();
    }

    // Inbound routing (Task 13): model request signals carry a JSON-encoded
    // ClientMessage; the BridgeFeed deserializes and pushes each onto the
    // bridge's inbound pump — the same path a WS client frame takes.
    Connections {
        target: blocklistsModel
        function onSubscriptionRequested(json) {
            bridgeFeed.sendClientJson(json);
        }
    }
    Connections {
        target: rulesModel
        function onRuleChangeRequested(json) {
            bridgeFeed.sendClientJson(json);
        }
    }

    // App-level bridge status. Hidden while the bridge is healthy; shows an
    // error banner over the current page if it failed to start. Floats above
    // pageStack so it's visible on any tab.
    Kirigami.InlineMessage {
        id: bridgeBanner
        z: 999
        anchors {
            top: parent.top
            left: parent.left
            right: parent.right
            margins: Kirigami.Units.smallSpacing
        }
        type: Kirigami.MessageType.Error
        visible: !bridgeFeed.ok
        text: bridgeFeed.statusText
    }

    // Page components, swapped into pageStack by the drawer actions below.
    Component {
        id: connectionsPageComponent
        ConnectionsPage {
            model: connectionsModel
            bridgeFeed: bridgeFeed
        }
    }
    Component {
        id: blocklistsPageComponent
        BlocklistsPage {
            model: blocklistsModel
            entriesModel: blocklistEntriesModel
        }
    }
    Component {
        id: rulesPageComponent
        RulesPage {
            model: rulesModel
        }
    }
    Component {
        id: trafficPageComponent
        TrafficPage {
            model: trafficModel
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
                onTriggered: root.pageStack.replace(trafficPageComponent)
            }
        ]
    }

    pageStack.initialPage: connectionsPageComponent
}
