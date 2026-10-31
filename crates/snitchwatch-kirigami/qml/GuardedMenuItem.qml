// A menu item that ignores a Space auto-repeat, like GuardedButton: Qt
// triggers a menu item on the release of an auto-repeated Space, so a key
// still held while the menu opens would press it.
import QtQuick
import QtQuick.Controls as Controls

Controls.MenuItem {
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
