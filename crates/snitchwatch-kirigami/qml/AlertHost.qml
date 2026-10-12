// Glue between the main window, the Settings values and the standalone alert
// (LS redesign phase 1), kept out of main.qml.
//
// * Publishes the Settings default duration as `Durations.defaultToken`.
// * Tells Rust (`BridgeFeed.setSilentState`) the silent mode and whether the
//   main window is shown: silent mode, or a hidden window, makes
//   `silent_mode.rs` answer new prompts at arrival.
// * Owns the alert, shown only while silent mode is off and the window shown.
//   Otherwise nobody would see it, and a prompt is answered at arrival.
import QtQuick
import com.snitchwatch.shell

Item {
    id: host

    // Injected by main.qml.
    property var settings: null
    property var bridgeFeed: null
    property var model: null
    property var window: null
    // Fixed text the alert could not send; main.qml shows it.
    signal notice(string text)

    property alias alert: alertWindow
    readonly property bool windowShown: host.window !== null && host.window.visible
    readonly property string mode: host.settings ? host.settings.silentMode : ""

    Binding {
        target: Durations
        property: "defaultToken"
        value: host.settings ? host.settings.defaultDuration : "forever"
    }

    function syncSilentState() {
        if (host.bridgeFeed !== null) {
            host.bridgeFeed.setSilentState(host.mode, host.windowShown);
        }
    }
    onModeChanged: host.syncSilentState()
    onWindowShownChanged: host.syncSilentState()
    Component.onCompleted: host.syncSilentState()

    AlertWindow {
        id: alertWindow
        model: host.model
        bridgeFeed: host.bridgeFeed
        active: (host.mode === "" || host.mode === "off") && host.windowShown
        onExplained: text => host.notice(text)
    }
}
