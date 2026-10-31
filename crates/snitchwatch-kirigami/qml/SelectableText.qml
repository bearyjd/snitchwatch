// Read-only text the mouse can select and copy (selectable-text plan, class
// A). A drop-in for a plain Controls.Label with these differences:
//   * no `elide` (a TextEdit cannot): wrap it, or keep a Label and give its
//     row a RowCopyMenu;
//   * `textFormat` defaults to PlainText (external text must never be
//     markup; callers still write `textFormat: Text.PlainText` explicitly,
//     which the honest-UI guards look for);
//   * mouse only: not a Tab stop and no focus ring, so the Tab itinerary and
//     the reading order are unchanged. A click does focus it, so Ctrl+C
//     works; the selection is kept when focus leaves, so the right-click menu
//     can still copy it;
//   * reports the StaticText role, not EditableText.
// Right-click opens "Copy" and "Select all" (made on first use).
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

TextEdit {
    id: root
    // The context menu once opened; null before.
    property var contextMenu: null

    readOnly: true
    selectByMouse: true
    persistentSelection: true
    activeFocusOnTab: false
    textFormat: TextEdit.PlainText
    wrapMode: TextEdit.Wrap
    color: Kirigami.Theme.textColor
    selectedTextColor: Kirigami.Theme.highlightedTextColor
    selectionColor: Kirigami.Theme.highlightColor
    Accessible.role: Accessible.StaticText
    Accessible.name: root.text

    HoverHandler {
        cursorShape: Qt.ArrowCursor
    }

    TapHandler {
        acceptedButtons: Qt.RightButton
        onTapped: point => {
            if (!root.contextMenu) {
                root.contextMenu = menuComponent.createObject(root);
            }
            root.contextMenu.popup(root, point.position.x, point.position.y);
        }
    }

    Component {
        id: menuComponent
        Controls.Menu {
            id: menu
            readonly property QtObject clipboard: ClipboardCopy {}
            GuardedMenuItem {
                objectName: "selectableCopy"
                text: "Copy"
                icon.name: "edit-copy"
                Accessible.name: "Copy"
                enabled: root.selectedText.length > 0
                onTriggered: menu.clipboard.copy(root.selectedText)
            }
            GuardedMenuItem {
                objectName: "selectableSelectAll"
                text: "Select all"
                Accessible.name: "Select all"
                onTriggered: root.selectAll()
            }
        }
    }
}
