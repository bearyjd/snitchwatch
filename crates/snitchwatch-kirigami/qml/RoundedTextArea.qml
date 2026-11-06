// A multi-line text box with the rounded look (see `RoundedField`): the same
// frame and `Theme.hitMin` floor, behaving exactly like `Controls.TextArea`.
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Controls.TextArea {
    id: control

    leftPadding: Kirigami.Units.largeSpacing
    rightPadding: Kirigami.Units.largeSpacing
    topPadding: Kirigami.Units.smallSpacing
    bottomPadding: Kirigami.Units.smallSpacing
    implicitHeight: Math.max(Theme.hitMin, contentHeight + topPadding + bottomPadding)

    background: RoundedFieldFrame {
        control: control
    }
}
