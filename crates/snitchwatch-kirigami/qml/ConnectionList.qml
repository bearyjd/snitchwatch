// The Connections list pane: the row ListView plus its empty-state message,
// split out of ConnectionsPage.qml so the page stays under its line budget
// (monitor-tree step 5).
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: pane

    // The owning ConnectionsPage.
    property var page
    property alias view: list
    Kirigami.Theme.colorSet: Kirigami.Theme.View
    Kirigami.Theme.inherit: false

    function openCurrent() {
        const item = list.currentItem;
        if (item && !item.isGroupHeader) {
            pane.page.openInspector(item);
        }
    }

    // Issue #114.1: the page is a plain Page (not a ScrollablePage, which
    // hides every child but its list), so this sibling of the list shows.
    Kirigami.PlaceholderMessage {
        objectName: "connectionsEmptyPlaceholder"
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: (page.model && page.model.totalCount > 0) ? "search" : "network-connect"
        text: (page.model && page.model.totalCount > 0)
              ? "No matching connections"
              : "No connections yet"
        explanation: (page.model && page.model.totalCount > 0)
              ? "No rows match the current filter."
              : "New connection prompts and recent decisions will appear here."
    }

    ListView {
        id: list
        anchors.fill: parent
        clip: true
        model: pane.page.model
        currentIndex: -1
        reuseItems: true
        Controls.ScrollBar.vertical: Controls.ScrollBar {}

        // Keyboard: Tab focuses the list, arrows move the selection (the wide
        // panel follows it), Enter opens it (the pushed page, when compact).
        focus: true
        activeFocusOnTab: true
        keyNavigationEnabled: true
        Keys.onReturnPressed: pane.openCurrent()
        Keys.onEnterPressed: pane.openCurrent()

        // Keep the Rust model's notion of the current selection in sync so its
        // auto-select policy knows whether the user is investigating a row.
        // In a wide window the inspector panel follows the selection.
        onCurrentIndexChanged: {
            const item = list.itemAtIndex(list.currentIndex);
            pane.page.model.setCurrentRowId(item ? item.rowId : "");
            pane.page.followSelection(item);
        }

        delegate: ConnectionRow {
            page: pane.page
        }
    }
}
