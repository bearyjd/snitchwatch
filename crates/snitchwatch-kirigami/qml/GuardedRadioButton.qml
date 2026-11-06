// A radio button that ignores a Space auto-repeat, as `GuardedSwitch` does for
// a switch: Qt takes an auto-repeated press as its own and clicks on the
// auto-repeat's release, so a key still held while the focus arrives or a
// page opens would change the choice. Used where the choice becomes a saved
// policy (the first-run silent-mode step). A fresh Space press still selects.
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.RadioButton {
    id: control
    implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
    background: RoundedSurface {
        control: control
        pill: false
        flat: true
    }
    indicator: Rectangle {
        implicitWidth: Theme.hitMin / 2
        implicitHeight: Theme.hitMin / 2
        x: control.text ? (control.mirrored ? control.width - width - control.rightPadding : control.leftPadding)
            : control.leftPadding + (control.availableWidth - width) / 2
        y: control.topPadding + (control.availableHeight - height) / 2
        radius: Theme.radiusPill
        color: "transparent"
        border.width: Theme.focusRingWidth
        border.color: control.checked ? control.palette.highlight : Qt.alpha(control.palette.windowText, 0.4)
        opacity: control.enabled ? 1 : Theme.disabledOpacity
        Rectangle {
            objectName: "radioDot"
            anchors.centerIn: parent
            width: parent.width / 2
            height: width
            radius: Theme.radiusPill
            color: control.palette.highlight
            visible: control.checked
        }
    }
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
