// A selector with a rounded body (Theme.radiusMedium) and a 44 px high
// hit area, iOS / Little Snitch 6 look. Behaves exactly like a plain
// `Controls.ComboBox` (same template, accessible role, popup and signals).
//
// It also carries the held-key guard every selector that saves its choice
// needs: an arrow, Home, End or Space still held (auto-repeating) must not
// walk through the choices, because each step would save one. The visuals are
// role-less and unfocusable: no extra Tab stop.
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.ComboBox {
    id: control

    implicitWidth: Math.max(Theme.hitMin, implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
    // The chevron's room is on the side it sits on: right, or left when mirrored.
    leftPadding: Theme.controlPaddingH / 2 + (mirrored ? Theme.hitMin / 2 : 0)
    rightPadding: Theme.controlPaddingH / 2 + (mirrored ? 0 : Theme.hitMin / 2)

    background: RoundedSurface {
        control: control
        pill: false
    }

    contentItem: Text {
        text: control.displayText
        font: control.font
        color: control.palette.buttonText
        opacity: control.enabled ? 1 : Theme.disabledOpacity
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }

    // A chevron: two arms of a rotated square.
    indicator: Item {
        objectName: "comboChevron"
        implicitWidth: Theme.hitMin / 2
        implicitHeight: Theme.hitMin / 2
        x: control.mirrored ? Theme.controlPaddingH / 4 : control.width - width - Theme.controlPaddingH / 4
        y: control.topPadding + (control.availableHeight - height) / 2
        opacity: control.enabled ? 1 : Theme.disabledOpacity
        Item {
            anchors.centerIn: parent
            width: parent.width / 3
            height: width
            rotation: 45
            Rectangle {
                width: Theme.focusRingWidth
                height: parent.height
                x: parent.width - width
                color: control.palette.buttonText
            }
            Rectangle {
                width: parent.width
                height: Theme.focusRingWidth
                y: parent.height - height
                color: control.palette.buttonText
            }
        }
    }

    Keys.priority: Keys.BeforeItem
    Keys.onPressed: event => { if (event.isAutoRepeat) event.accepted = true; }
    Keys.onReleased: event => { if (event.isAutoRepeat) event.accepted = true; }
}
