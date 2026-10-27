// Connections filter bar: the Pending only / Denied only / From now / Full
// list switches, the exception-first controls (BackgroundGroupBar) and the
// filter summary. Hosted as the page's `header`, above the list and
// outside the fixed-height title row, so a wrapping Flow pushes the list down
// instead of covering its first rows. Every switch is bound to the model's
// state, so it follows external changes; a toggle forwards to the model and
// re-establishes its binding. The matching and the summary text are decided
// in Rust (connections/filter.rs).
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: bar
    property var model

    implicitHeight: column.implicitHeight + Kirigami.Units.smallSpacing * 2

    Column {
        id: column
        x: Kirigami.Units.largeSpacing
        y: Kirigami.Units.smallSpacing
        width: bar.width - Kirigami.Units.largeSpacing * 2
        spacing: Kirigami.Units.smallSpacing

        // Wraps onto further lines at narrow widths instead of clipping.
        Flow {
            width: column.width
            spacing: Kirigami.Units.largeSpacing

            Controls.Switch {
                id: pendingOnly
                text: "Pending only"
                checked: bar.model ? bar.model.filterPendingOnly : false
                onToggled: {
                    bar.model.setPendingOnly(checked)
                    checked = Qt.binding(() => bar.model ? bar.model.filterPendingOnly : false)
                }
            }
            Controls.Switch {
                id: deniedOnly
                text: "Denied only"
                checked: bar.model ? bar.model.filterDeniedOnly : false
                onToggled: {
                    bar.model.setDeniedOnly(checked)
                    checked = Qt.binding(() => bar.model ? bar.model.filterDeniedOnly : false)
                }
            }
            Controls.Switch {
                id: fromNow
                readonly property bool on: bar.model ? bar.model.filterSinceMs > 0 : false
                text: on
                      ? "From now (since " + Qt.formatTime(new Date(bar.model.filterSinceMs), "hh:mm:ss") + ")"
                      : "From now"
                checked: on
                onToggled: {
                    bar.model.setFromNow(checked)
                    checked = Qt.binding(() => fromNow.on)
                }
            }
            // Off by default: handled rows collapse into "Background and
            // system" (clarity 0c). On shows every row.
            Controls.Switch {
                id: fullList
                objectName: "fullListSwitch"
                text: "Full list"
                checked: bar.model ? !bar.model.filterNeedsAttention : false
                onToggled: {
                    bar.model.setNeedsAttention(!checked)
                    checked = Qt.binding(() => bar.model ? !bar.model.filterNeedsAttention : false)
                }
            }
        }

        BackgroundGroupBar {
            width: column.width
            model: bar.model
        }

        Controls.Label {
            width: column.width
            visible: bar.model ? bar.model.filterSummary.length > 0 : false
            text: bar.model ? "Filter: " + bar.model.filterSummary : ""
            wrapMode: Text.WordWrap
            color: Kirigami.Theme.disabledTextColor
        }
    }
}
