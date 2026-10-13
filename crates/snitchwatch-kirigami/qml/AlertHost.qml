// Glue between the main window, the Settings values and the standalone alert
// (LS redesign phase 1), kept out of main.qml.
//
// * Publishes the Settings default duration as `Durations.defaultToken`.
// * Tells Rust (`BridgeFeed.setSilentState`) the silent mode: Allow/Deny make
//   `silent_mode.rs` answer prompts without asking (new ones at arrival,
//   waiting ones when the mode is switched).
// * Owns the alert, shown whenever silent mode is off ("Ask me"). It is its
//   own window, independent of the main window: it asks whether that window
//   is visible or hidden to the tray, and nothing is answered for the user.
import QtQuick
import com.snitchwatch.shell

Item {
    id: host

    // Injected by main.qml.
    property var settings: null
    property var bridgeFeed: null
    property var model: null
    // Fixed text the alert could not send; main.qml shows it.
    signal notice(string text)

    property alias alert: alertWindow
    // The alert is up and has keyboard focus: the user is looking at it.
    // main.qml hands it to NotificationController, which posts a desktop
    // notice for a prompt still waiting if this is false after the grace period.
    readonly property bool alertFocused: alertWindow.visible
        && alertWindow.activeFocusItem !== null
    readonly property string mode: host.settings ? host.settings.silentMode : ""

    Binding {
        target: Durations
        property: "defaultToken"
        value: host.settings ? host.settings.defaultDuration : "forever"
    }

    function syncSilentState() {
        if (host.bridgeFeed !== null) {
            host.bridgeFeed.setSilentState(host.mode);
        }
    }
    onModeChanged: host.syncSilentState()
    Component.onCompleted: host.syncSilentState()

    AlertWindow {
        id: alertWindow
        model: host.model
        bridgeFeed: host.bridgeFeed
        active: host.mode === "" || host.mode === "off"
        onExplained: text => host.notice(text)
    }
}
