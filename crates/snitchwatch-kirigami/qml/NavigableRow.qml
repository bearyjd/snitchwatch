// A list row for the list pages (Q01): an ItemDelegate that is one Tab stop,
// that the Up and Down keys step between (focus moves to the neighbouring
// row's delegate, whatever inside the row had it), and that Return opens like
// a click. What a key does is decided by the row that HAS FOCUS when it is
// pressed, never by the list's `currentItem`, which a click or an update can
// leave on another row.
//
// A Space auto-repeat is ignored (Qt clicks a button on the release of one,
// and takes an auto-repeated press as its own): a key still held when focus
// arrives, or when a sheet opens over the row, must not open or press
// anything. Return opens the row only on a fresh press.
import QtQuick
import QtQuick.Controls as Controls

Controls.ItemDelegate {
    id: row

    required property int index

    Keys.priority: Keys.BeforeItem
    Keys.onPressed: event => {
        if (event.key === Qt.Key_Space && event.isAutoRepeat) {
            event.accepted = true;
        }
    }
    Keys.onReleased: event => {
        if (event.key === Qt.Key_Space && event.isAutoRepeat) {
            event.accepted = true;
        }
    }
    Keys.onUpPressed: event => { event.accepted = row.focusNeighbour(-1); }
    Keys.onDownPressed: event => { event.accepted = row.focusNeighbour(1); }
    Keys.onReturnPressed: event => row.openByKey(event)
    Keys.onEnterPressed: event => row.openByKey(event)

    function openByKey(event) {
        event.accepted = true;
        if (!event.isAutoRepeat) row.clicked();
    }

    // Focus the row `step` away (the first or last at either end).
    function focusNeighbour(step) {
        const view = row.ListView.view;
        if (!view) return false;
        const target = Math.max(0, Math.min(view.count - 1, row.index + step));
        view.positionViewAtIndex(target, ListView.Contain);
        const next = view.itemAtIndex(target);
        if (next) next.forceActiveFocus(Qt.TabFocusReason);
        return true;
    }
}
