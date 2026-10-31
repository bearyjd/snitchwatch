// A tool button that ignores a Space auto-repeat (see GuardedButton). Qt
// clicks a button on the release of an auto-repeat, so a key still held
// while the focus arrives would apply a scope or move the camera. A held
// key's own final release still clicks it once; Return and Enter never do.
import QtQuick
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Controls.ToolButton {
    id: button
    implicitWidth: Math.max(Theme.hitMin, implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
    background: RoundedSurface {
        control: button
        flat: true
        tinted: button.checked
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
