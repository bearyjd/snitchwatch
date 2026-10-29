// A button that ignores a Space auto-repeat. Qt's button clicks on the release
// of an auto-repeat, and takes an auto-repeated press as its own, so a key
// still held while the focus arrives, a dialog opens or the rows are replaced
// would press it. Used for what cannot be taken back: Apply, Start and Stop
// learning, Cancel and "Yes, delete". Return and Enter never click a Qt
// button; a held key's own final release still clicks it once.
import QtQuick
import QtQuick.Controls as Controls

Controls.Button {
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
}
