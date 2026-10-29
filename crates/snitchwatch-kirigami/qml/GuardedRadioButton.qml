// A radio button that ignores a Space auto-repeat, as `GuardedSwitch` does for
// a switch: Qt takes an auto-repeated press as its own and clicks on the
// auto-repeat's release, so a key still held while the focus arrives or a
// page opens would change the choice. Used where the choice becomes a saved
// policy (the first-run silent-mode step). A fresh Space press still selects.
import QtQuick
import QtQuick.Controls as Controls

Controls.RadioButton {
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
