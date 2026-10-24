// Page-owned, captured targets and a virtual list keep large IP stacks usable.
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Popup {
    id: popup
    property var members: []
    signal selected(var node)
    width: Math.min(400, parent ? parent.width : 400)
    height: Math.min(360, members.length * 48 + 16)
    padding: 8
    function popup(anchor) {
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
    contentItem: ListView {
        clip: true
        model: popup.members
        currentIndex: -1
        function chooseCurrent() {
            if (currentIndex>=0 && currentIndex<popup.members.length) {
                popup.selected(popup.members[currentIndex]);
                popup.close();
            }
        }
        Keys.onReturnPressed: chooseCurrent()
        Keys.onEnterPressed: chooseCurrent()
        keyNavigationEnabled: true
        activeFocusOnTab: true
        Controls.ScrollBar.vertical: Controls.ScrollBar {}
        delegate: Controls.ItemDelegate {
            required property var modelData
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
    onOpened: contentItem.forceActiveFocus()
}
