// A plain-text hover tooltip for the control it sits in (declare it as a
// child). The style's attached `ToolTip.text` is AutoText, so markup in data
// (a process or host name) would render; this one draws a PlainText label, as
// SizedOverlaySheet's title tooltip does. Set `onlyWhen` to e.g. `truncated`
// to show it only for elided text.
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Item {
    id: root
    anchors.fill: parent

    property string text: ""
    property bool onlyWhen: true

    HoverHandler {
        id: hover
    }

    Controls.ToolTip {
        id: tip
        visible: root.onlyWhen && root.text !== "" && hover.hovered
        // Rounded like the rest of the app; the native ToolTip elsewhere
        // (the style's own) is left to the platform.
        background: Rectangle {
            radius: Theme.radiusSmall
            color: tip.palette.toolTipBase
            border.width: Theme.borderWidth
            border.color: Qt.alpha(tip.palette.toolTipText, Theme.borderOpacity)
        }
        contentItem: Controls.Label {
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            // The style's own tooltip text uses the tooltip palette.
            color: palette.toolTipText
            text: root.text
        }
    }
}
