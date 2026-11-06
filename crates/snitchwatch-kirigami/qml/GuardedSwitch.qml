// A switch that ignores a Space auto-repeat, as `GuardedButton` does for a
// button: Qt clicks a switch on the release of an auto-repeat and takes an
// auto-repeated press as its own, so a key still held while the focus
// arrives or a page opens would flip it. Used for what turns something on or
// off that is not just a view setting: a rule group, launch at login, online
// research. Return and Enter never flip a Qt switch; a held key's own final
// release still flips it once.
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.Switch {
    id: control
    implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
    background: RoundedSurface {
        control: control
        pill: false
        flat: true
    }
    // iOS track and knob; the row around them carries hover and the focus ring.
    indicator: Item {
        implicitWidth: Theme.switchTrackWidth
        implicitHeight: Theme.switchTrackHeight
        x: control.text ? (control.mirrored ? control.width - width - control.rightPadding : control.leftPadding)
            : control.leftPadding + (control.availableWidth - width) / 2
        y: control.topPadding + (control.availableHeight - height) / 2
        Rectangle {
            objectName: "switchTrack"
            anchors.fill: parent
            radius: Theme.radiusPill
            color: control.checked ? control.palette.highlight : Qt.alpha(control.palette.windowText, 0.2)
            opacity: control.enabled ? 1 : Theme.disabledOpacity
        }
        Rectangle {
            objectName: "switchKnob"
            readonly property int inset: (Theme.switchTrackHeight - Theme.switchKnob) / 2
            width: Theme.switchKnob
            height: Theme.switchKnob
            radius: Theme.radiusPill
            y: inset
            x: inset + control.visualPosition * (parent.width - width - 2 * inset)
            color: control.checked ? control.palette.highlightedText : control.palette.base
            border.width: Theme.borderWidth
            border.color: Qt.alpha(control.palette.windowText, Theme.borderOpacity)
            opacity: control.enabled ? 1 : Theme.disabledOpacity
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
