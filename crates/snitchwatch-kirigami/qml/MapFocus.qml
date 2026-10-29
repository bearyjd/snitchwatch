// Keyboard focus for the map. A new scope replaces the markers, and the one
// with the focus goes with it; a popup's opener may be gone when it closes.
// Either way a keyboard user stays in the map, never on nothing.
import QtQuick
import QtQuick.Window

QtObject {
    id: keeper
    property Item mapView: null
    property MapDestinationsPopup menu: null

    // The window holding the map, once it has one.
    readonly property var win: mapView ? mapView.Window.window : null
    function focusInside() {
        for (let it = win ? win.activeFocusItem : null; it; it = it.parent) {
            if (it === mapView) return true;
        }
        return false;
    }
    function nothingFocused() {
        return !win || !win.activeFocusItem || win.activeFocusItem === win.contentItem;
    }
    function keep() {
        if (nothingFocused() && !menu.visible) mapView.forceActiveFocus();
    }
    function returnFocus() {
        if (!nothingFocused()) return;
        const opener = menu.opener;
        if (opener && opener.visible && opener.enabled) opener.forceActiveFocus();
        else mapView.forceActiveFocus();
    }
    // A marker was activated; it may be gone before a deferred call runs,
    // so the deferral lives here.
    function activate(cluster, marker) {
        const hadFocus = marker.activeFocus;
        mapView.activate(cluster, marker);
        if (hadFocus) Qt.callLater(keeper.keep);
    }
    property Connections menuWatch: Connections {
        target: keeper.menu
        function onClosed() { Qt.callLater(keeper.returnFocus); }
    }
}
