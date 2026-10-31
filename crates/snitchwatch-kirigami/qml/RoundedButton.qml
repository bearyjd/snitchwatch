// A capsule button, 44 px high at least, with hover / pressed / focus
// feedback (iOS / Little Snitch 6 look). Behaves exactly like a plain
// `Controls.Button`; for what cannot be taken back use `GuardedButton`, which
// extends this and also ignores a Space auto-repeat.
import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Controls.impl as Impl
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Controls.Button {
    id: control

    implicitWidth: Math.max(Theme.hitMin, implicitContentWidth + leftPadding + rightPadding)
    implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
    horizontalPadding: Theme.controlPaddingH
    verticalPadding: Kirigami.Units.smallSpacing
    spacing: Kirigami.Units.smallSpacing
    icon.color: highlighted ? palette.highlightedText : palette.buttonText

    contentItem: Impl.IconLabel {
        spacing: control.spacing
        mirrored: control.mirrored
        display: control.display
        icon: control.icon
        text: control.text
        font: control.font
        color: control.highlighted ? control.palette.highlightedText : control.palette.buttonText
    }

    background: RoundedSurface {
        control: control
        prominent: control.highlighted
        tinted: control.checked
        flat: control.flat
    }
}
