// A single-line text box with the rounded look: `Theme.radiusSmall` corners and
// never under `Theme.hitMin` (44 px) high, so a field in a dense list is as easy
// to hit as a button. Behaves exactly like a plain `Controls.TextField`: same
// root type, Keys handlers and Accessible properties pass straight through, and
// the frame adds no Tab stop. The padding is symmetric, so a right-to-left
// layout mirrors the text with nothing to overlap.
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Controls.TextField {
    id: control

    leftPadding: Kirigami.Units.largeSpacing
    rightPadding: Kirigami.Units.largeSpacing
    topPadding: Kirigami.Units.smallSpacing
    bottomPadding: Kirigami.Units.smallSpacing
    verticalAlignment: TextInput.AlignVCenter
    implicitHeight: Math.max(Theme.hitMin, contentHeight + topPadding + bottomPadding)

    background: RoundedFieldFrame {
        control: control
    }
}
