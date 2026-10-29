// Page-owned, captured targets and a virtual list keep large IP stacks usable.
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Popup {
    id: popup
    property var members: []
    // The control that opened the list, for the focus to return to.
    property Item opener: null
    signal selected(var node)
    width: Math.min(400, parent ? parent.width : 400)
    height: Math.min(360, members.length * 48 + 16)
    padding: 8
    function popup(anchor) {
        opener = anchor;
        if (anchor && parent) {
            const point = anchor.mapToItem(parent, 0, anchor.height);
            x = Math.max(0, Math.min(parent.width - width, point.x));
            y = Math.max(0, Math.min(parent.height - height, point.y));
        } else {
            x = 0;
            y = 0;
        }
        open();
    }
    // Choose one entry: always the node itself, taken when the key is
    // pressed, never an index into a list that may have moved.
    function choose(node) {
        popup.selected(node);
        popup.close();
    }
    contentItem: ListView {
        clip: true
        model: popup.members
        currentIndex: -1
        Accessible.name: "Places"
        Accessible.role: Accessible.List
        keyNavigationEnabled: true
        activeFocusOnTab: true
        Controls.ScrollBar.vertical: Controls.ScrollBar {}
        delegate: Controls.ItemDelegate {
            required property var modelData
            id: entry
            highlighted: ListView.isCurrentItem
            Accessible.name: modelData.name + (modelData.summary ? " (" + modelData.summary + ")" : "")
            // Space clicks on its release, auto-repeats included: choose on
            // a fresh press instead, and ignore a held key.
            Keys.priority: Keys.BeforeItem
            Keys.onPressed: event => {
                if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space) {
                    if (!event.isAutoRepeat) popup.choose(entry.modelData);
                    event.accepted = true;
                }
            }
            width: ListView.view.width
            height: Math.max(48, implicitHeight)
            contentItem: Controls.Label {
                text: modelData.name + (modelData.summary ? " (" + modelData.summary + ")" : "")
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
            }
            onClicked: {
                popup.selected(modelData);
                popup.close();
            }
        }
    }
    onOpened: {
        contentItem.currentIndex = -1;
        contentItem.forceActiveFocus();
        contentItem.currentIndex = members.length > 0 ? 0 : -1;
    }
}
