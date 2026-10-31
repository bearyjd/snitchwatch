// A button that ignores a Space auto-repeat. Qt's button clicks on the release
// of an auto-repeat, and takes an auto-repeated press as its own, so a key
// still held while the focus arrives, a dialog opens or the rows are replaced
// would press it. Used for what cannot be taken back: Apply, Start and Stop
// learning, Cancel and "Yes, delete". Return and Enter never click a Qt
// button; a held key's own final release still clicks it once. A button that
// sets `returnActivates` (the learning banner's Stop learning and Review) is
// also clicked by a fresh Return or Enter press; an auto-repeated one never.
import QtQuick

RoundedButton {
    id: button
    property bool returnActivates: false
    Keys.priority: Keys.BeforeItem
    Keys.onPressed: event => {
        if (button.returnActivates && (event.key === Qt.Key_Return || event.key === Qt.Key_Enter)) {
            if (!event.isAutoRepeat && button.enabled) {
                button.clicked();
            }
            event.accepted = true;
            return;
        }
        if (event.key === Qt.Key_Space && event.isAutoRepeat) {
            event.accepted = true;
        }
    }
    Keys.onReleased: event => {
        if (event.key === Qt.Key_Space && event.isAutoRepeat) {
            event.accepted = true;
        }
    }
}
