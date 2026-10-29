// A switch that ignores a Space auto-repeat, as `GuardedButton` does for a
// button: Qt clicks a switch on the release of an auto-repeat and takes an
// auto-repeated press as its own, so a key still held while the focus
// arrives or a page opens would flip it. Used for what turns something on or
// off that is not just a view setting: a rule group, launch at login, online
// research. Return and Enter never flip a Qt switch; a held key's own final
// release still flips it once.
import QtQuick
import QtQuick.Controls as Controls

Controls.Switch {
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
