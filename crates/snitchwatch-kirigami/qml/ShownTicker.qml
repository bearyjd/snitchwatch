import QtQuick

// main.qml's 1 s banner tick: the bridge banner and the prompt-slot banner's
// wait time. Display only, so it runs only while the window is shown (not
// hidden to the tray, not minimized), and catches up at once when it is
// shown again (perf/gui-idle-wakeups). `ok`/`linkState`, which main.qml's
// resets and the verdict gates read, are also refreshed on every link change
// by BridgeFeed's own feed, so they stay current while hidden.
Timer {
    id: ticker
    required property var window
    required property var feed
    required property var slot
    readonly property bool shown: ticker.window.visibility !== Window.Hidden
        && ticker.window.visibility !== Window.Minimized
    interval: 1000
    running: ticker.shown
    repeat: true
    triggeredOnStart: true
    onTriggered: {
        ticker.feed.refresh();
        ticker.slot.refresh();
    }
}
