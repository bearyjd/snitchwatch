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
import Qt.labs.platform as Labs
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
    // the single inbound sink the models' request signals feed. The external
    // bridge client is started in `main.rs` before this QML loads; `refresh()`
    // reflects its reconnecting service state.
    BridgeFeed {
        id: bridgeFeed
    }
    // Who holds opensnitchd's single prompt slot (PromptSlotBanner below).
    PromptSlotStatus {
        id: promptSlotStatus
    }
    // The service can restart independently of this GUI. Polling is cheap
    // (one mutex-backed string read) and makes the existing banner accurately
    // report both disconnects and reconnects without adding a Qt event bridge.
    Timer {
        interval: 1000
        running: true
        repeat: true
        onTriggered: {
            bridgeFeed.refresh();
            // Also ticks the prompt-slot banner's wait time.
            promptSlotStatus.refresh();
        }
    }
    // QML ids are lexical names, not properties on `root`. Components below
    // need explicitly named root properties to inject these objects without
    // accidentally self-binding a same-named page property.
    property var bridgeFeedRef: bridgeFeed
    // Exposed for the headless probe (tests/honest_ui_main_banner_qml.rs).
    property alias bridgeStatusLabel: bridgeStatusLabel

    // Core-loop connection model (Task 6). Owned here so its lifetime spans the
    // window; pages bind to it. Its live outbound feed is started in the
    // window's Component.onCompleted via startBridgeFeed().
    ConnectionsModel {
        id: connectionsModel
    }
    property var connectionsModelRef: connectionsModel
    // For page components, whose own `geoModel` property would shadow the id.
    property var geoModelRef: geoModel
    // Page navigation that first pops any pushed page (see StackNav.qml).
    StackNav {
        id: nav
        stack: root.pageStack
    }

    // "Make a rule…" requests (PR #111 review, OQ1). Owned here, not by the
    // Connections page: the drawer's pageStack.replace destroys that page, and
    // a request's outcome must still arrive. The notice is fixed text only.
    MakeRuleController {
        id: makeRuleController
        Component.onCompleted: startBridgeFeed()
    }
    property var makeRuleControllerRef: makeRuleController
    MakeRuleOutcomes {
        controller: makeRuleController
        shownInPlace: function (rowId) {
            const page = root.pageStack.currentItem;
            return !!page && !!page.makeRuleSheet && page.makeRuleSheet.shows(rowId);
        }
        onNotice: text => root.showPassiveNotification(text, "long")
    }

    // Per-country geographic breakdown (Geo panel). Same lifetime/ownership
    // shape as connectionsModel; outbound feed started alongside the others
    // below via startBridgeFeed().
    GeoModel {
        id: geoModel
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

    // Profiles tab model (switchable "At Home"/"Public Wi-Fi"/"Office"
    // firewall profiles, with network-based auto-activation on the bridge
    // side). Outbound feed started below; profile-change request signal
    // routed to the bridge inbound via the Connections block further down.
    ProfilesModel {
        id: profilesModel
    }

    // Recommended background-service rules (prompt-slot D): opt-in rules the
    // bridge adds and removes; the request signal is routed below.
    CuratedDefaultsModel {
        id: curatedDefaultsModel
    }

    Connections {
        target: bridgeFeed
        function onOkChanged() { if (!bridgeFeed.ok) curatedDefaultsModel.resetBridgeSession(); }
    }

    // First-run onboarding wizard (Task 12). Owns daemon-detection state; the
    // onboarding page itself is pushed/popped below based on `state`.
    WizardController {
        id: wizardController
    }

    // Settings & Diagnostics (Tasks 15, 16). Autostart toggle + crash-log
    // viewer; both are plain-file-backed, no bridge dependency.
    SettingsController {
        id: settingsController
    }
    // The rule buttons' states depend on the default duration; the model
    // re-reads it (once) when Settings publish a change.
    Connections {
        target: settingsController
        function onDefaultDurationChanged() { connectionsModel.refreshLevelRuleDuration(); }
    }

    // Desktop notification dispatch (Task 17). `windowActive` is a live
    // binding so the 5-second-grace-period check in Rust always sees this
    // window's current active state, not a stale snapshot.
    NotificationController {
        id: notificationController
        windowActive: root.active
        // A prompt of this session is asked by the alert; if the alert is not
        // the active window after the grace period, a notice is the fallback.
        alertActive: alertHost.alertFocused
    }

    // Tray icon state feed (Task 18). The actual `SystemTrayIcon` is declared
    // further down; this object only derives its tooltip/menu-label text from
    // the bridge's `TrayState`.
    TrayController {
        id: trayController
    }

    // Component B's privileged-tier scanner report (Phase 6). No bridge
    // dependency — triggers an on-demand pkexec-gated deep scan and exposes
    // its JSON report; see scanner_controller.rs's module doc.
    ScannerController {
        id: scannerController
    }

    // Daemon/kernel readiness diagnostics (Task 8/9). Polls the bridge for
    // opensnitchd reachability and kernel prerequisite health; the banner
    // and DaemonHealthPage below both bind to this.
    DaemonHealthModel {
        id: daemonHealthModel
        Component.onCompleted: startBridgeFeed()
    }

    // Guards against pushing the onboarding page more than once and against
    // popping when it was never pushed (e.g. "Continue anyway" already
    // dismissed it before a stray stateChanged fires).
    property bool onboardingShown: false

    Component {
        id: onboardingPageComponent
        OnboardingPage {
            controller: wizardController
            onDismissed: {
                root.onboardingShown = false;
                root.pageStack.pop();
            }
        }
    }

    // Show the wizard only when detection actually finds a problem — the
    // controller's optimistic default (`connected`) means a healthy daemon
    // never triggers this at all (no `stateChanged` fires if probe() confirms
    // the default). Never blocks the window permanently: `OnboardingPage`'s
    // "Continue anyway" button and this same handler's pop-on-connected path
    // both let the user past it.
    Connections {
        target: wizardController
        function onStateChanged() {
            if (wizardController.state !== "connected" && !root.onboardingShown) {
                root.onboardingShown = true;
                root.pageStack.push(onboardingPageComponent);
            } else if (wizardController.state === "connected" && root.onboardingShown) {
                root.onboardingShown = false;
                root.pageStack.pop();
            }
        }
    }

    // Task 13 live wiring. The bridge is already started (main.rs); here we (a)
    // reflect its status into the banner, and (b) start each model's outbound
    // feed task. startBridgeFeed()/refresh() are no-ops when the bridge failed
    // to start, so a degraded bridge still yields a working (if empty) window.
    Component.onCompleted: {
        bridgeFeed.refresh();
        bridgeFeed.startBridgeFeed();
        connectionsModel.startBridgeFeed();
        blocklistsModel.startBridgeFeed();
        blocklistEntriesModel.startBridgeFeed();
        rulesModel.startBridgeFeed();
        trafficModel.startBridgeFeed();
        profilesModel.startBridgeFeed();
        curatedDefaultsModel.startBridgeFeed();
        geoModel.startBridgeFeed();
        wizardController.probe();
        settingsController.refreshPreferences();
        notificationController.startBridgeFeed();
        trayController.startBridgeFeed();
        promptSlotStatus.startBridgeFeed();
        // SNITCHWATCH_DEMO=1 only (the model ignores the call otherwise).
        if (appInfo.demoMode) {
            connectionsModel.loadDemoRows();
            geoModel.loadDemoRows();
            curatedDefaultsModel.loadDemoGroups();
        }
    }

    // Review screenshots (demo mode only); see ScreenshotRunner.qml.
    ScreenshotRunner {
        window: root
        dir: appInfo.screenshotDir
        alertWindow: alertHost.alert
        pages: ({
            connections: connectionsPageComponent,
            rules: rulesPageComponent,
            blocklists: blocklistsPageComponent,
            curatedGroups: curatedGroupsPageComponent,
            recommendedRules: recommendedRulesPageComponent
        })
    }

    // The standalone alert and the silent-mode/duration wiring (LS redesign
    // phase 1); see AlertHost.qml.
    AlertHost {
        id: alertHost
        settings: settingsController
        bridgeFeed: bridgeFeed
        model: connectionsModel
        onNotice: text => root.showPassiveNotification(text, "long")
    }

    // First-run step asking how new connections are treated when nobody is
    // asked. Shown once, while nothing has been chosen yet.
    property bool silentStepShown: false
    Component {
        id: silentModePageComponent
        SilentModePage {
            settings: settingsController
            onDone: {
                root.silentStepShown = false;
                // Offer the rule groups next, only when the bridge sends them.
                if (curatedDefaultsModel.grouped) {
                    root.pageStack.replace(curatedGroupsPageComponent);
                } else {
                    root.pageStack.pop();
                }
            }
            onReviewRecommendedRules: {
                root.silentStepShown = false;
                root.pageStack.replace(recommendedRulesPageComponent);
            }
        }
    }
    Component {
        id: curatedGroupsPageComponent
        CuratedGroupsPage {
            model: curatedDefaultsModel
            onDone: root.pageStack.pop()
        }
    }
    Connections {
        target: settingsController
        function onPreferencesLoadedChanged() {
            if (settingsController.silentMode === "" && !appInfo.demoMode && !root.silentStepShown) {
                root.silentStepShown = true;
                root.pageStack.push(silentModePageComponent);
            }
        }
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
    Connections {
        target: profilesModel
        function onProfileChangeRequested(json) {
            bridgeFeed.sendClientJson(json);
        }
    }
    Connections {
        target: curatedDefaultsModel
        function onCuratedChangeRequested(json) {
            bridgeFeed.sendClientJson(json);
        }
    }
    Connections {
        target: trayController
        function onFilteringToggleRequested(json) {
            bridgeFeed.sendClientJson(json);
        }
    }

    // App-level bridge status. Hidden while the bridge is healthy; otherwise
    // one fixed sentence for the link state the Rust runtime reports
    // (`bridgeFeed.linkState`: connecting, retrying, failed or stopped), so the
    // banner never claims the client keeps retrying while it is only starting
    // or after it gave up. Floats above pageStack so it's visible on any tab.
    // `statusText` carries the bridge runtime's own error message, so it sits
    // in a PlainText label under the fixed-text message, never in it (issue
    // #51: InlineMessage renders its text as markup), and the sentence is
    // never chosen from it.
    ColumnLayout {
        id: bridgeBanner
        z: 999
        anchors {
            top: parent.top
            left: parent.left
            right: parent.right
            margins: Kirigami.Units.smallSpacing
        }
        visible: !bridgeFeed.ok
        spacing: 0

        Kirigami.InlineMessage {
            objectName: "bridgeMessage-connecting"
            Layout.fillWidth: true
            visible: bridgeFeed.linkState === "connecting"
            type: Kirigami.MessageType.Information
            text: "Snitchwatch is connecting to its background service."
        }
        Kirigami.InlineMessage {
            objectName: "bridgeMessage-retrying"
            Layout.fillWidth: true
            visible: bridgeFeed.linkState === "retrying"
            type: Kirigami.MessageType.Warning
            text: "Snitchwatch can't reach its background service. It keeps retrying."
        }
        Kirigami.InlineMessage {
            objectName: "bridgeMessage-failed"
            Layout.fillWidth: true
            visible: bridgeFeed.linkState === "failed"
            type: Kirigami.MessageType.Error
            text: "Snitchwatch couldn't start its connection to the background service. Restart Snitchwatch to try again."
        }
        Kirigami.InlineMessage {
            objectName: "bridgeMessage-stopped"
            Layout.fillWidth: true
            visible: bridgeFeed.linkState === "stopped"
            type: Kirigami.MessageType.Error
            text: "Snitchwatch's connection to its background service has ended. Restart Snitchwatch to connect again."
        }
        Rectangle {
            Layout.fillWidth: true
            implicitHeight: bridgeStatusLabel.implicitHeight + Kirigami.Units.smallSpacing * 2
            color: Kirigami.Theme.backgroundColor

            Controls.Label {
                id: bridgeStatusLabel
                anchors.fill: parent
                anchors.margins: Kirigami.Units.smallSpacing
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                text: bridgeFeed.statusText
            }
        }
    }

    // Daemon/kernel readiness banner — distinct from bridgeBanner above:
    // bridgeBanner covers the bridge PROCESS failing to start; this covers
    // the bridge running fine but opensnitchd/kernel prerequisites not
    // being met. See docs/superpowers/specs/2026-07-14-daemon-kernel-diagnostics-design.md.
    Kirigami.InlineMessage {
        id: daemonHealthBanner
        z: 999
        anchors {
            top: bridgeBanner.visible ? bridgeBanner.bottom : parent.top
            left: parent.left
            right: parent.right
            margins: Kirigami.Units.smallSpacing
        }
        type: Kirigami.MessageType.Warning
        visible: daemonHealthModel.hasProblem
        text: daemonHealthModel.statusSummary
        actions: [
            Kirigami.Action {
                text: "Details"
                onTriggered: nav.replaceMain(daemonHealthPageComponent)
            }
        ]
    }

    // Pending-decision-exposure warning — distinct from both banners above:
    // this covers neither the bridge nor opensnitchd being unhealthy, but a
    // known *upstream* opensnitchd limitation (evilsocket/opensnitch#1644):
    // its AskRule dispatch serializes on a single global flag, so while any
    // one decision is outstanding, every other new connection silently gets
    // the daemon's DefaultAction applied with no signal Snitchwatch can
    // observe. This banner only narrows the exposure window (by prompting
    // the user to respond) — it cannot detect or close it. See
    // docs/superpowers/plans/2026-08-05-pending-decision-exposure-warning.md.
    Kirigami.InlineMessage {
        id: pendingExposureBanner
        z: 999
        anchors {
            top: daemonHealthBanner.visible ? daemonHealthBanner.bottom
                : (bridgeBanner.visible ? bridgeBanner.bottom : parent.top)
            left: parent.left
            right: parent.right
            margins: Kirigami.Units.smallSpacing
        }
        type: Kirigami.MessageType.Warning
        readonly property int pendingAgeThresholdSecs: 10
        // opensnitchd caps a single AskRule at 120s and unconditionally
        // clears isAsking once that fires (vendor/opensnitch/daemon/ui/
        // client.go:366, main.go:458-459) — past that, the exposure window
        // this banner warns about is already closed even though a row can
        // stay "pending" in Snitchwatch's own cache indefinitely on a bridge
        // older than #39 (such as the published v0.1.1 tarball), which has no
        // reaper for a cancelled/dropped verdict oneshot. Newer bridges remove
        // the row when the ask is cancelled; this ceiling stays for the older
        // ones. Without this ceiling the banner would count up forever and
        // its claim would go from true to false with no visible change —
        // worse than no banner, since it trains the user to ignore a real
        // warning.
        readonly property int pendingAgeCeilingSecs: 120
        readonly property int pendingAgeSecs: root.connectionsModelRef.oldestPendingAgeSecs
        readonly property int pendingCount: root.connectionsModelRef.pendingCount
        // A bridge that reports the prompt slot gets PromptSlotBanner instead.
        visible: !promptSlotStatus.supported
            && pendingAgeSecs >= pendingAgeThresholdSecs && pendingAgeSecs < pendingAgeCeilingSecs
        // Deliberately does not say "silently allowed": that's only true
        // under DefaultAction: allow, opensnitchd's own fail-open default.
        // This repo's shipped packaging config overrides that to
        // DefaultAction: deny (fail-closed — the system flavour's staged
        // config under packaging/system/, CLAUDE.md's settled
        // decision #5), so on the actual shipped product the real effect is
        // "silently denied" (a legitimate connection blackholed with no
        // signal) — arguably worse, not better. Neither direction is
        // hardcoded here since the configured default isn't threaded to the
        // UI (Codex review finding); "allowed or denied" stays accurate
        // regardless of DefaultAction.
        text: (pendingCount > 1
                ? (pendingCount + " decisions are pending (oldest " + pendingAgeSecs + "s)")
                : ("A decision has been pending for " + pendingAgeSecs + "s"))
            + ". Until you respond, other new connections may be silently allowed or denied"
            + " without your review — this is a known opensnitchd limitation, not a"
            + " Snitchwatch bug."
    }

    // Prompt-slot plan, part A, and issue #78: which prompt holds the slot,
    // for how long, what it costs, and that a pause can't get past it. Only
    // from a bridge that reports it; the estimate above covers older ones.
    PromptSlotBanner {
        id: promptSlotBanner
        z: 999
        anchors {
            top: daemonHealthBanner.visible ? daemonHealthBanner.bottom
                : (bridgeBanner.visible ? bridgeBanner.bottom : parent.top)
            left: parent.left
            right: parent.right
            margins: Kirigami.Units.smallSpacing
        }
        status: promptSlotStatus
        model: root.connectionsModelRef
        bridgeFeed: root.bridgeFeedRef
        onExplained: text => root.showPassiveNotification(text, "long")
        onReviewRequested: nav.replaceMain(connectionsPageComponent)
    }

    // Keeps ConnectionsModel.oldestPendingAgeSecs (and therefore
    // pendingExposureBanner above) live: that property only changes
    // automatically on the next bridge message, so elapsed wall-clock time
    // otherwise needs an explicit poke. Declared at window scope, not as a
    // child of the banner it drives — the banner is invisible in exactly the
    // state (age < threshold) where this timer's ticks matter most, and a Qt
    // child-of-invisible-item is not guaranteed to keep behaving identically
    // across every InlineMessage implementation. Only runs while something
    // is *actively* pending (Codex review: gating on pendingCount > 0 alone
    // ticks forever once a stuck/expired row exists — see
    // oldest_pending_started_at_ms's doc comment in row_store.rs — since a
    // stuck row never resolves and never expires from pendingCount either;
    // oldestPendingAgeSecs is already -1 in exactly that case, so gating on
    // it here stops the wakeups once the real exposure window closes,
    // regardless of whether the stuck row itself is ever cleaned up).
    Timer {
        interval: 1000
        running: root.connectionsModelRef.oldestPendingAgeSecs >= 0
        repeat: true
        onTriggered: root.connectionsModelRef.refreshPendingAge()
    }

    // Page components, swapped into pageStack by the drawer actions below.
    Component {
        id: connectionsPageComponent
        ConnectionsPage {
            model: root.connectionsModelRef
            geoModel: root.geoModelRef
            bridgeFeed: root.bridgeFeedRef
            makeRuleController: root.makeRuleControllerRef

            // Rule-match diagnostics "Show rule" jump (Parity 4): navigate to
            // the Rules tab and open the matched rule's detail sheet
            // directly, using the page instance `nav.replaceMain` returns
            // rather than a separate lookup/scroll step.
            onShowRuleRequested: function(ruleName) {
                const rulesPage = nav.replaceMain(rulesPageComponent);
                rulesPage.openRuleByName(ruleName);
            }

            // "Simulate this connection": same route, to the Simulate sheet,
            // prefilled with what the inspected row carries.
            onSimulateConnectionRequested: function(prefillJson) {
                const rulesPage = nav.replaceMain(rulesPageComponent);
                rulesPage.openSimulator(prefillJson);
            }
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
            curatedModel: curatedDefaultsModel
        }
    }
    Component {
        id: trafficPageComponent
        TrafficPage {
            model: trafficModel
        }
    }
    Component {
        id: profilesPageComponent
        ProfilesPage {
            model: profilesModel
        }
    }
    Component {
        id: recommendedRulesPageComponent
        RecommendedRulesPage {
            model: curatedDefaultsModel
        }
    }
    Component {
        id: diagnosticsPageComponent
        DiagnosticsPage {
            controller: settingsController
        }
    }
    Component {
        id: geoPageComponent
        GeoPage {
            model: geoModel
        }
    }
    Component {
        id: scannerPageComponent
        ScannerPage {
            controller: scannerController
        }
    }
    Component {
        id: daemonHealthPageComponent
        DaemonHealthPage {
            model: daemonHealthModel
        }
    }
    // A new prompt does NOT raise this window. The alert (AlertHost.qml ->
    // AlertWindow.qml) is its own window, independent of this one: in "Ask me"
    // it asks whether this window is shown or hidden to the tray (if the alert
    // is still not the active window after 5 s, NotificationController posts
    // a desktop notice for the prompt), and in
    // "Allow/Deny and record" the prompt is answered silently, so raising the
    // main window for it would be the interruption silent mode exists to avoid.
    // `raiseAndActivate()` below is still how the tray and a notification's
    // "Review" action bring this window back.

    // The rule editor for a connection (P2.1): go to the Rules tab and open
    // the editor prefilled from the simulator's prefill form
    // (`ConnectionsModel.simulationPrefillJson`), the way "Simulate this
    // connection" opens the Simulate sheet. Returns whether it opened.
    function openRuleEditor(prefillJson) {
        const rulesPage = nav.replaceMain(rulesPageComponent);
        return rulesPage.openEditor(prefillJson);
    }

    // Shared raise/focus helper (Task 7 requirement 1). Used by the tray and
    // by the "Review" action on a desktop notification (Task 17): one place
    // that needs the manual fullscreen-focus verification (2026-07-11, real
    // Plasma Wayland session; see the 2026-07-04 plan's Task 7 checklist).
    // Under the compositor's focus-stealing prevention it may be refused.
    function raiseAndActivate() {
        if (root.visibility === Window.Minimized || root.visibility === Window.Hidden)
            root.showNormal();
        root.raise();
        root.requestActivate();
    }

    // Task 17's fallback path: the user clicked "Review" on a desktop
    // notification. Bring the window back.
    Connections {
        target: notificationController
        function onReviewRequested() {
            root.raiseAndActivate();
        }
    }

    // Task 18 tray icon. `Qt.labs.platform.SystemTrayIcon` (verified present
    // at /usr/lib64/qt6/qml/Qt/labs/platform in this environment) renders via
    // the platform's StatusNotifierItem support on Plasma — no cxx-qt/KDE
    // Frameworks binding layer needed.
    Labs.SystemTrayIcon {
        id: trayIcon
        visible: true
        icon.name: "security-high"
        tooltip: trayController.tooltip

        onActivated: function (reason) {
            if (reason === Labs.SystemTrayIcon.Trigger) {
                root.visible ? root.hide() : root.raiseAndActivate();
            }
        }

        // Flat menu in TrayMenu.qml: a nested submenu doesn't survive the
        // StatusNotifierItem/dbusmenu export on Plasma.
        menu: TrayMenu {
            controller: trayController
            window: root
        }
    }

    // Close-to-tray (Task 18), matching `snitchwatch-tauri`'s
    // `on_window_event`/`CloseRequested` handler: closing the window hides it
    // instead of quitting, since the tray icon is the only way back in
    // otherwise. `Qt.quit()` above (tray menu) is the actual exit path.
    // A hidden window changes nothing about prompts: the alert is its own
    // window and still asks in "Ask me" mode.
    onClosing: function (close) {
        close.accepted = false;
        root.hide();
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
                onTriggered: nav.replaceMain(connectionsPageComponent)
            },
            Kirigami.Action {
                text: "Geography"
                icon.name: "map-globe"
                onTriggered: nav.replaceMain(geoPageComponent)
            },
            Kirigami.Action {
                text: "Rules"
                icon.name: "view-list-details"
                onTriggered: nav.replaceMain(rulesPageComponent)
            },
            Kirigami.Action {
                text: "Blocklists"
                icon.name: "edit-delete"
                onTriggered: nav.replaceMain(blocklistsPageComponent)
            },
            Kirigami.Action {
                text: "Traffic"
                icon.name: "office-chart-line"
                onTriggered: nav.replaceMain(trafficPageComponent)
            },
            Kirigami.Action {
                text: "Profiles"
                icon.name: "preferences-system-network"
                onTriggered: nav.replaceMain(profilesPageComponent)
            },
            Kirigami.Action {
                text: "Recommended background-service rules"
                icon.name: "checkmark"
                onTriggered: nav.replaceMain(recommendedRulesPageComponent)
            },
            Kirigami.Action {
                text: "Security Scan"
                icon.name: "security-low"
                onTriggered: nav.replaceMain(scannerPageComponent)
            },
            Kirigami.Action {
                text: "Settings & Diagnostics"
                icon.name: "settings-configure"
                onTriggered: nav.replaceMain(diagnosticsPageComponent)
            },
            Kirigami.Action {
                text: "Daemon Health"
                icon.name: "dialog-warning"
                onTriggered: nav.replaceMain(daemonHealthPageComponent)
            }
        ]
    }

    pageStack.initialPage: connectionsPageComponent
}
