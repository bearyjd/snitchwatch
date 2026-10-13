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

    Kirigami.PlaceholderMessage {
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
