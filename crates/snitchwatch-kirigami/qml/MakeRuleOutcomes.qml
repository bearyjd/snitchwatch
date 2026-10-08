// "Make a rule…" requests, for as long as the window lives (PR #111 review,
// OQ1): the drawer replaces pages, which destroys the Connections page and
// its sheet, so the controller and this watch live in main.qml. It polls the
// controller while a request waits, and says how a request ended when its
// row isn't on screen.
//
// The notice is FIXED text only (PR #111 review, H1): a passive notification
// renders rich text, and a refusal's reason comes from the bridge. The reason
// stays in the sheet's plain-text result.
import QtQuick

QtObject {
    id: outcomes

    property var controller: null
    // Whether `rowId`'s outcome is already on screen (the inspector's sheet).
    property var shownInPlace: function (rowId) {
        return false;
    }

    readonly property string createdText: "The rule was created."
    readonly property string notCreatedText: "A rule couldn't be created. Open that connection to see why."

    // One of the two fixed texts above, for the window's passive notification.
    signal notice(string text)

    // The bridge's result never came: give up after a silence.
    property Timer poller: Timer {
        interval: 1000
        repeat: true
        running: !!outcomes.controller && outcomes.controller.busy
        onTriggered: outcomes.controller.poll()
    }

    property Connections watcher: Connections {
        target: outcomes.controller
        function onFinished(rowId, created) {
            if (!outcomes.shownInPlace(rowId)) {
                outcomes.notice(created ? outcomes.createdText : outcomes.notCreatedText);
            }
        }
    }
}
