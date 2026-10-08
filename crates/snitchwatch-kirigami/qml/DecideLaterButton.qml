// "Decide later" on a pending row (prompt-slot plan Part C, owner decision
// S2): the bridge blocks the program for 5 minutes on any host, then it is
// asked about again. Without a program file the bridge can name, the
// firewall's default action applies to this connection instead. Shown only
// where the row's bridge session takes it (InlineVerdicts.rowDecideLater).
//
// The explanation is fixed text in an explicit PlainText contentItem, never
// the attached `ToolTip.text` (issue #51).
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Button {
    id: button
    flat: true
    text: "Decide later"
    icon.name: "chronometer-pause"

    readonly property string explanation: "Blocks this program for 5 minutes, then asks again. If its file is unknown, the firewall's default action applies to this connection instead."
    Accessible.description: button.explanation

    Controls.ToolTip {
        visible: button.hovered
        delay: Kirigami.Units.toolTipDelay
        contentItem: Controls.Label {
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: palette.toolTipText
            text: button.explanation
        }
    }
}
