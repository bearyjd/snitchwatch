// The rounded drawing every restyled control shares (iOS / Little Snitch 6
// look): a soft fill, a hairline, a hover / pressed wash and a rounded focus
// ring. It is only ever the `background:` of a control, so it has no input of
// its own: it takes no clicks, is never focused and has no accessible role.
//
// It replaces the platform style's drawing for that control, including the
// style's own focus frame, so the ring below is the keyboard focus indicator.
// It shows for keyboard focus only (`visualFocus`), as the platform's does.
//
// Radii, sizes and opacities come from `Theme`; colours from the control's own
// palette, so light and dark colour schemes both work.
import QtQuick
import com.snitchwatch.shell

Item {
    id: surface

    // The control this is the background of.
    required property Item control
    // A capsule (buttons) or `Theme.radiusMedium` (rows, switches).
    property bool pill: true
    // No resting fill or hairline (tool buttons, switch and radio rows).
    property bool flat: false
    // The primary action: solid selection colour.
    property bool prominent: false
    // A chosen toggle: a tinted fill.
    property bool tinted: false

    readonly property int cornerRadius: pill ? Theme.radiusPill : Theme.radiusMedium
    readonly property color ink: control.palette.windowText
    readonly property alias body: body
    readonly property alias focusRing: ring
    readonly property alias wash: wash
    // The ring is chosen against the fill it sits on: a primary button is
    // filled with `highlight`, so a `highlight` ring would vanish on it.
    readonly property color ringColor: prominent ? control.palette.highlightedText
        : control.palette.highlight

    implicitWidth: Theme.hitMin
    implicitHeight: Theme.hitMin
    opacity: control.enabled ? 1 : Theme.disabledOpacity

    Rectangle {
        id: body
        anchors.fill: parent
        radius: surface.cornerRadius
        visible: !surface.flat || surface.tinted || surface.prominent
        color: surface.prominent ? surface.control.palette.highlight
            : surface.tinted ? Qt.alpha(surface.control.palette.highlight, 0.2)
            : Qt.alpha(surface.ink, Theme.fillOpacity)
        border.width: surface.prominent || surface.flat ? 0 : Theme.borderWidth
        border.color: Qt.alpha(surface.ink, Theme.borderOpacity)
    }

    // Hover and pressed wash.
    Rectangle {
        id: wash
        objectName: "stateWash"
        anchors.fill: parent
        radius: surface.cornerRadius
        color: surface.prominent ? surface.control.palette.highlightedText : surface.ink
        opacity: surface.control.down ? Theme.pressedOpacity
            : surface.control.hovered ? Theme.hoverOpacity : 0
    }

    // Keyboard focus ring (`ringColor` contrasts with the fill), inside the bounds so a clipping parent never hides it.
    Rectangle {
        id: ring
        objectName: "focusRing"
        anchors.fill: parent
        radius: surface.cornerRadius
        color: "transparent"
        visible: surface.control.visualFocus
        border.width: Theme.focusRingWidth
        border.color: surface.ringColor
    }
}
