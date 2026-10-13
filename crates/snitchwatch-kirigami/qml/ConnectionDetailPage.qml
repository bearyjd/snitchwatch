// The inspector as a stacked page, pushed onto the page stack by
// ConnectionsPage when the window is too narrow for the side column (monitor-tree
// step 5). Hosts the same InspectorPanel; popping it (back) just returns to the list.
import QtQuick
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: detail

    // The owning ConnectionsPage.
    property var page
    // Exposed like ConnectionsPage's own aliases, so main.qml's
    // `pageStack.currentItem.makeRuleSheet` finds it while this page is on top.
    property alias panel: panel
    property alias makeRuleSheet: panel.makeRuleSheet
    title: page ? page.inspectProcess : ""

    InspectorPanel {
        id: panel
        page: detail.page
        showHeader: false
    }

    // Popped by Back: ConnectionsPage notices (its stack's depth changed and
    // this page is gone) and ends the selection. The destruction below covers a
    // pop that depth didn't reveal. Losing focus or being covered (a second
    // column in a mid-width window, an onboarding page pushed on top) is NOT
    // leaving.
    Component.onDestruction: {
        if (page && page.detailPage === detail) {
            page.detailPageGone();
        }
    }
}
