// A check box with a rounded box (Theme.radiusSmall) and a 44 px high row,
// iOS / Little Snitch 6 look. Behaves exactly like a plain `Controls.CheckBox`
// (same template, Keys, accessible role, name and checked state); the row
// around the box carries hover and the rounded focus ring. The visuals are
// role-less and unfocusable: no extra Tab stop.
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.CheckBox {
    id: control

    implicitWidth: Math.max(Theme.hitMin, implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)

    background: RoundedSurface {
        control: control
        pill: false
        flat: true
    }

    indicator: Rectangle {
        objectName: "checkBox"
        implicitWidth: Theme.hitMin / 2
        implicitHeight: Theme.hitMin / 2
        x: control.text ? (control.mirrored ? control.width - width - control.rightPadding : control.leftPadding)
            : control.leftPadding + (control.availableWidth - width) / 2
        y: control.topPadding + (control.availableHeight - height) / 2
        radius: Theme.radiusSmall
        color: control.checkState === Qt.Unchecked ? "transparent" : control.palette.highlight
        border.width: Theme.focusRingWidth
        border.color: control.checkState === Qt.Unchecked
            ? Qt.alpha(control.palette.windowText, 0.4) : control.palette.highlight
        opacity: control.enabled ? 1 : Theme.disabledOpacity

        // The tick: a rotated L (short and long arm), or a bar when partly checked.
        Item {
            objectName: "checkTick"
            anchors.centerIn: parent
            width: parent.width / 2
            height: parent.height / 2
            visible: control.checkState === Qt.Checked
            rotation: 45
            Rectangle {
                width: Theme.focusRingWidth
                height: parent.height
                x: parent.width - width
                color: control.palette.highlightedText
            }
            Rectangle {
                width: parent.width * 0.6
                height: Theme.focusRingWidth
                y: parent.height - height
                color: control.palette.highlightedText
            }
        }
        Rectangle {
            objectName: "checkPartial"
            anchors.centerIn: parent
            width: parent.width / 2
            height: Theme.focusRingWidth
            visible: control.checkState === Qt.PartiallyChecked
            color: control.palette.highlightedText
        }
    }

    contentItem: Text {
        leftPadding: control.indicator && !control.mirrored ? control.indicator.width + control.spacing : 0
        rightPadding: control.indicator && control.mirrored ? control.indicator.width + control.spacing : 0
        text: control.text
        font: control.font
        color: control.palette.windowText
        opacity: control.enabled ? 1 : Theme.disabledOpacity
        elide: Text.ElideRight
        verticalAlignment: Text.AlignVCenter
    }
}
