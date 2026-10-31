// Read-only text the mouse can select and copy (selectable-text plan, class
// A). A drop-in for a plain Controls.Label with these differences:
//   * no `elide` (a TextEdit cannot): wrap it, or keep a Label and give its
//     row a RowCopyMenu;
//   * `textFormat` defaults to PlainText (external text must never be
//     markup; callers still write `textFormat: Text.PlainText` explicitly,
//     which the honest-UI guards look for);
//   * mouse only: it never takes active focus (a click or drag selects without
//     moving focus), so the Tab itinerary, the arrow keys and the reading order
//     are unchanged. Ctrl+C is a Shortcut on the one text that holds the
//     selection (SelectionRegistry), off while a text field has focus;
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
    activeFocusOnPress: false
    focus: false
    textFormat: TextEdit.PlainText
    wrapMode: TextEdit.Wrap
    color: Kirigami.Theme.textColor
    selectedTextColor: Kirigami.Theme.highlightedTextColor
    selectionColor: Kirigami.Theme.highlightColor
    Accessible.role: Accessible.StaticText
    Accessible.name: root.text

    HoverHandler {
        cursorShape: Qt.IBeamCursor
    }

    // The last text selected owns Ctrl+C and clears the others.
    onSelectedTextChanged: {
        if (root.selectedText.length === 0) {
            return;
        }
        const previous = SelectionRegistry.current;
        SelectionRegistry.current = root;
        if (previous && previous !== root) {
            previous.deselect();
        }
    }
    Component.onDestruction: {
        if (SelectionRegistry.current === root) {
            SelectionRegistry.current = null;
        }
    }

    // A focused text control keeps its own Ctrl+C.
    readonly property bool fieldHasFocus: {
        const f = root.Window.activeFocusItem;
        return !!f && f !== root && f.selectedText !== undefined
            && (!f.readOnly || f.selectedText.length > 0);
    }
    Shortcut {
        sequences: [StandardKey.Copy]
        enabled: root.visible && SelectionRegistry.current === root
            && root.selectedText.length > 0 && !root.fieldHasFocus
        onActivated: root.copy()
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
