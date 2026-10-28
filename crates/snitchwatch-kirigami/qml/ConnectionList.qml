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

    // Phase 0b: the headers' "last hour" / "today" counts age with the clock
    // even when no row arrives; one coarse tick rolls them forward.
    Timer {
        interval: 60000
        running: !!pane.page && !!pane.page.model
        repeat: true
        triggeredOnStart: true
        onTriggered: pane.page.model.refreshVerdictWindows()
    }

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
        readonly property bool allCollapsed: !!page.model && page.model.filterNeedsAttention
              && page.model.backgroundCount > 0 && !page.model.filterSummary
        text: (page.model && page.model.totalCount > 0)
              ? (allCollapsed ? "Nothing needs attention" : "No matching connections")
              : "No connections yet"
        explanation: (page.model && page.model.totalCount > 0)
              ? (allCollapsed ? "Background and system connections are collapsed above."
                              : "No rows match the current filter.")
              : "New connection prompts and recent decisions will appear here."
    }

    ListView {
        id: list
        anchors.fill: parent
        clip: true
        model: pane.page.model
        currentIndex: -1
        reuseItems: true
        // Qt 6.10/6.11 can cancel a stale async cache index after a shrinking
        // model reset. Render visible rows synchronously; keep delegate reuse.
        cacheBuffer: 0
        Controls.ScrollBar.vertical: Controls.ScrollBar {}

        // Explicit map selection belongs to the list that materializes the
        // target. Keep the inspector/form guard and re-resolve the stable ID.
        function selectGeographicRow(id) {
            if (pane.page.inspecting) return;
            const index=pane.page.model.visibleRowIndex(id);
            if (index<0) return;
            list.forceLayout();list.currentIndex=index;
            const item=list.itemAtIndex(index);
            if (item && !item.isGroupHeader) pane.page.openInspector(item);
        }

        // Keyboard: Tab focuses the list, arrows move the selection (the wide
        // panel follows it), Enter opens it (the pushed page, when compact).
        focus: true
        activeFocusOnTab: true
        keyNavigationEnabled: true
        Keys.onReturnPressed: pane.openCurrent()
        Keys.onEnterPressed: pane.openCurrent()
        // Q01: the Menu key or Shift+F10 opens a row's actions: the row
        // whose button has focus (keys bubble up to the list), else the
        // selected row; nothing with no row. Below the row; closing the menu
        // gives focus back (Popup).
        // Focus on the list itself: on the selected row, never on a button
        // the list's focus scope last held.
        function focusCurrent() {
            (list.currentItem || list).forceActiveFocus(Qt.BacktabFocusReason);
        }
        function rowOf(item) {
            for (let it = item; it; it = it.parent) {
                if (it.parent === list.contentItem) return it;
            }
            return null;
        }
        Keys.onPressed: event => {
            const menuKey = event.key === Qt.Key_Menu
                || (event.key === Qt.Key_F10 && (event.modifiers & Qt.ShiftModifier));
            // A delegate itself holding the list's focus is not a button in
            // it: then the selected row counts.
            const focused = list.Window.activeFocusItem;
            const inRow = list.rowOf(focused);
            const row = !menuKey ? null : inRow && inRow !== focused ? inRow : list.currentItem;
            if (row) {
                pane.page.connectionActions.open(row, 0, row.height);
                event.accepted = true;
            }
        }
        // Tab from a selected row goes to its own buttons (a header's
        // Allow all / Deny all, a pending row's Allow), not the first row's.
        Keys.onTabPressed: event => {
            const stop = list.currentItem ? list.currentItem.firstTabStop : null;
            event.accepted = !!stop && stop.visible && stop.enabled;
            if (event.accepted) stop.forceActiveFocus(Qt.TabFocusReason);
        }

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
