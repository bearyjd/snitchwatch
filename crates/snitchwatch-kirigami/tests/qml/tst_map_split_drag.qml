// Pins how Qt's vertical SplitView behaves under a real handle drag, the way
// MonitorPane.qml uses it (map band above the tree; drag to zero collapses it;
// the tree keeps a minimum). Run with `just qml-test`.
//
// A structural MIRROR of MonitorPane.qml + MapBand.qml's height logic with
// plain items (qmltestrunner cannot load com.snitchwatch.shell; see
// tst_delegate_input.qml). tests/map_view_qml.rs covers the real band's
// followDrag(); tests/honest_ui_qml_guards.rs pins the real wiring.
import QtQuick
import QtQuick.Controls as Controls
import QtTest

Item {
    id: root
    width: 400
    height: 600

    readonly property real header: 30
    readonly property real chrome: 80
    readonly property real minBody: 60
    readonly property real treeMin: 100
    property bool collapsed: false
    property real bodyHeight: 120

    function followDrag(h) {
        const body = h - root.chrome;
        root.collapsed = body < root.minBody / 2;
        if (!root.collapsed) root.bodyHeight = Math.max(root.minBody, body);
    }
    readonly property real preferred: root.collapsed ? root.header : root.chrome + root.bodyHeight

    Controls.SplitView {
        id: split
        anchors.fill: parent
        orientation: Qt.Vertical
        onResizingChanged: if (!split.resizing) band.Controls.SplitView.preferredHeight = root.preferred
        handle: Rectangle { implicitHeight: 8; color: "gray" }

        Rectangle {
            id: band
            color: "lightblue"
            Controls.SplitView.minimumHeight: split.resizing ? root.header
                : (root.collapsed ? root.header : root.chrome + root.minBody)
            Controls.SplitView.maximumHeight: split.resizing || !root.collapsed
                ? Math.max(root.header, split.height - root.treeMin - 8) : root.header
            Controls.SplitView.preferredHeight: root.preferred
            onHeightChanged: if (split.resizing) root.followDrag(band.height)
        }
        Rectangle {
            id: tree
            color: "lightgreen"
            Controls.SplitView.fillHeight: true
            Controls.SplitView.minimumHeight: root.treeMin
        }
    }

    TestCase {
        name: "MapSplitDrag"
        when: windowShown

        function handleY() { return band.height + 4; }

        function init() {
            root.collapsed = false;
            root.bodyHeight = 120;
            band.Controls.SplitView.preferredHeight = root.preferred;
            wait(50);
        }

        function test_drag_to_zero_collapses_and_drag_out_expands() {
            compare(band.height, 200);
            mouseDrag(root, 200, handleY(), 0, -300, Qt.LeftButton);
            wait(50);
            verify(root.collapsed, "dragging to zero must collapse the band");
            compare(band.height, root.header);
            mouseDrag(root, 200, handleY(), 0, 120, Qt.LeftButton);
            wait(50);
            verify(!root.collapsed, "dragging back out must expand it");
            verify(band.height >= root.chrome + root.minBody, "expanded band keeps its minimum");
        }

        function test_band_never_takes_the_trees_minimum() {
            mouseDrag(root, 200, handleY(), 0, 500, Qt.LeftButton);
            wait(50);
            verify(tree.height >= root.treeMin, "tree height " + tree.height);
        }

        function test_short_window_shrinks_the_band_first() {
            root.height = 330;
            wait(50);
            compare(tree.height >= root.treeMin, true, "tree " + tree.height);
            compare(band.height >= root.chrome + root.minBody, true, "band " + band.height);
            root.height = 600;
        }
    }
}
