// The rounded frame behind `RoundedField` and `RoundedTextArea` (iOS / Little
// Snitch 6 look): a `base` fill, a hairline and a rounded focus ring. It is
// only ever the `background:` of a text control, so it takes no input, is never
// focused and has no accessible role.
//
// Unlike a button, a text box shows its ring whenever it has the keyboard
// (`activeFocus`), by mouse or by Tab, as every text box does. The ring is
// `highlight` on a `base` fill, never a prominent fill, and sits inside the
// bounds so a clipping parent never hides it.
import QtQuick
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: frame

    // The text control this is the background of.
    required property Item control

    readonly property int cornerRadius: Theme.radiusSmall
    readonly property alias body: body
    readonly property alias focusRing: ring

    // A usable width of its own (the platform style supplies about this much),
    // so a field in a row with no `Layout.fillWidth` does not collapse.
    implicitWidth: Kirigami.Units.gridUnit * 11
    implicitHeight: Theme.hitMin
    opacity: control.enabled ? 1 : Theme.disabledOpacity

    Rectangle {
        id: body
        anchors.fill: parent
        radius: frame.cornerRadius
        color: frame.control.palette.base
        border.width: Theme.borderWidth
        border.color: Qt.alpha(frame.control.palette.windowText,
            frame.control.hovered ? Theme.borderOpacity * 2 : Theme.borderOpacity)
    }

    Rectangle {
        id: ring
        objectName: "focusRing"
        anchors.fill: parent
        radius: frame.cornerRadius
        color: "transparent"
        visible: frame.control.activeFocus
        border.width: Theme.focusRingWidth
        border.color: frame.control.palette.highlight
    }
}
