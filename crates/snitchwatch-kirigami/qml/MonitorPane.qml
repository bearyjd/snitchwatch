// The Monitor's left pane (LS redesign phase 2, step 9): the featured map band
// above the connections tree in a vertical SplitView. Dragging the handle
// resizes the band; dragging it to (nearly) zero snaps it collapsed to its
// header row. A short pane takes height from the band first, down to its
// minimum, before the tree loses any. Split out of ConnectionsPage.qml to keep
// that file under its line budget.
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: pane

    // The owning ConnectionsPage (its geoModel and model feed the band).
    property var page
    property alias view: tree.view
    property alias mapBand: band
    // The tree is never given less than this by the band. The split's own
    // height already excludes the page's filter bar and rule bar, so no header
    // height is subtracted here; the band's real chrome is in band.chromeHeight.
    readonly property real treeMinimum: Kirigami.Units.gridUnit * 8
    readonly property real handleHeight: Kirigami.Units.smallSpacing * 1.5
    // A handle drag overwrites the band's preferred height; take it back.
    function applyPreferred() {
        band.Controls.SplitView.preferredHeight = band.preferredHeight;
    }

    Controls.SplitView {
        id: split
        anchors.fill: parent
        orientation: Qt.Vertical
        onResizingChanged: if (!split.resizing) pane.applyPreferred()

        handle: Rectangle {
            implicitHeight: pane.handleHeight
            color: Controls.SplitHandle.pressed ? Kirigami.Theme.highlightColor
                : (Controls.SplitHandle.hovered ? Qt.alpha(Kirigami.Theme.highlightColor, 0.5)
                                                : Kirigami.Theme.backgroundColor)
            Accessible.name: "Resize map"
            Accessible.role: Accessible.Separator
            Rectangle {
                anchors.centerIn: parent
                width: Kirigami.Units.gridUnit * 3
                height: 3
                radius: 1.5
                color: Kirigami.Theme.textColor
                opacity: 0.4
            }
        }

        MapBand {
            id: band
            geoModel: pane.page.geoModel
            connectionsModel: pane.page.model
            // While dragging, the band may go all the way to its header row.
            Controls.SplitView.minimumHeight: split.resizing ? band.collapsedHeight : band.minimumHeight
            Controls.SplitView.maximumHeight: Math.min(
                split.resizing ? band.chromeHeight + band.maximumBody : band.maximumHeight,
                Math.max(split.resizing ? band.collapsedHeight : band.minimumHeight,
                    split.height - pane.treeMinimum - pane.handleHeight))
            Controls.SplitView.preferredHeight: band.preferredHeight
            onHeightChanged: if (split.resizing) band.followDrag(band.height)
            onPreferredHeightChanged: if (!split.resizing) pane.applyPreferred()
        }

        ConnectionList {
            id: tree
            Controls.SplitView.fillHeight: true
            Controls.SplitView.minimumHeight: pane.treeMinimum
            page: pane.page
        }
    }
}
