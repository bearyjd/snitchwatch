// Pins how Qt's vertical SplitView behaves under a real handle drag, the way
// MonitorPane.qml uses it (map band above the tree; drag to zero collapses it;
// the tree keeps a minimum). Run with `just qml-test`.
//
// A structural MIRROR of MonitorPane.qml + MapBand.qml's height logic with
// plain items; the SplitView expressions are copied verbatim from the real
// files and pinned by honest_ui_qml_guards.rs (qmltestrunner cannot load com.snitchwatch.shell; see
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
    readonly property real maxBody: 200
    readonly property real handleHeight: 8
    property bool collapsed: false
    property real bodyHeight: 120

    function followDrag(h) {
        const body = h - root.chrome;
        root.collapsed = body < root.minBody / 2;
        if (!root.collapsed) root.bodyHeight = Math.max(root.minBody, body);
    }
    readonly property real preferred: root.collapsed ? root.header : root.chrome + root.bodyHeight
    readonly property real minimumHeight: root.collapsed ? root.header : root.chrome + root.minBody
    readonly property real maximumHeight: root.collapsed ? root.header : root.chrome + root.maxBody

    onPreferredChanged: if (!split.resizing) band.Controls.SplitView.preferredHeight = root.preferred

    Controls.SplitView {
        id: split
        anchors.fill: parent
        orientation: Qt.Vertical
        onResizingChanged: if (!split.resizing) band.Controls.SplitView.preferredHeight = root.preferred
        handle: Rectangle { implicitHeight: root.handleHeight; color: "gray" }

        Rectangle {
            id: band
            color: "lightblue"
            Controls.SplitView.minimumHeight: split.resizing ? root.header : root.minimumHeight
            Controls.SplitView.maximumHeight: Math.min(
                split.resizing ? root.chrome + root.maxBody : root.maximumHeight,
                Math.max(split.resizing ? root.header : root.minimumHeight,
                    split.height - root.treeMin - root.handleHeight))
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

        function handleY() { return band.height + root.handleHeight / 2; }

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
            root.height = 280;
            wait(50);
            verify(band.height < root.preferred, "band " + band.height + " vs preferred " + root.preferred);
            verify(band.height >= root.minimumHeight, "band below its minimum: " + band.height);
            compare(tree.height, root.treeMin);
            root.height = 600;
        }

        // The bug the first mirror missed: once collapsed, the cap must not
        // pin the handle at the header, or the band cannot be dragged open.
        function test_collapsed_band_can_be_dragged_open() {
            root.collapsed = true;
            band.Controls.SplitView.preferredHeight = root.preferred;
            wait(50);
            compare(band.height, root.header);
            mouseDrag(root, 200, handleY(), 0, 150, Qt.LeftButton);
            wait(50);
            verify(!root.collapsed, "a collapsed band must reopen when dragged out");
            verify(band.height > root.header + 100, "band height " + band.height);
        }
    }
}
