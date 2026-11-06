// Right-click "Copy" for a clickable list row (selectable-text plan, class
// C). Put one inside the row delegate; `copyText` is the plain text the
// caller wants on the clipboard (a row's text cannot be drag-selected: that
// would fight the row's click and the list's flick). A handler, not an
// item: it adds no Tab stop and no node to the reading order, and it takes
// the right button only, so the row's own click is untouched. Mouse only:
// the Menu key and keyboard Copy buttons are left to the row and the page.
// The menu is made on the first right-click.
import QtQuick
import QtQuick.Controls as Controls

TapHandler {
    id: handler
    objectName: "rowCopyMenu"
    // The plain text "Copy" puts on the clipboard.
    property string copyText: ""
    // The menu once opened; null before.
    property var menu: null
    acceptedButtons: Qt.RightButton
    onTapped: point => {
        if (!handler.menu) {
            handler.menu = menuComponent.createObject(handler.parent);
        }
        handler.menu.popup(handler.parent, point.position.x, point.position.y);
    }

    readonly property Component menuComponent: Component {
        Controls.Menu {
            id: menu
            readonly property QtObject clipboard: ClipboardCopy {}
            GuardedMenuItem {
                objectName: "rowCopyItem"
                text: "Copy"
                icon.name: "edit-copy"
                Accessible.name: "Copy"
                onTriggered: menu.clipboard.copy(handler.copyText)
            }
        }
    }
}
