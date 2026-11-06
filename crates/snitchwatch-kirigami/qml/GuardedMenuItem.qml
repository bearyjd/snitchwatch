// A menu item that ignores a Space auto-repeat, like GuardedButton: Qt
// triggers a menu item on the release of an auto-repeated Space, so a key
// still held while the menu opens would press it.
import QtQuick
import QtQuick.Controls as Controls

Controls.MenuItem {
    // Every tappable is at least Theme.hitMin (44 px) in both directions, even
    // under a compact desktop style whose own menu rows are shorter.
    implicitWidth: Math.max(Theme.hitMin, implicitBackgroundWidth + leftInset + rightInset,
        implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Math.max(Theme.hitMin, implicitBackgroundHeight + topInset + bottomInset,
        implicitContentHeight + topPadding + bottomPadding)

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
