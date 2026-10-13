// Connections page header, split out of ConnectionsPage.qml (monitor-tree
// step 1) with no behaviour change.
//
// Search and the filter switches live in the page header so they stay visible
// while the list scrolls. Every control only forwards to the model: the token
// syntax, the matching and the filter summary are all decided in Rust
// (connections/filter.rs).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: header
    // The owning ConnectionsPage supplies the model and the title.
    property var model
    property string title

    Layout.fillWidth: true
    spacing: Kirigami.Units.smallSpacing

    RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Heading {
            text: header.title
            level: 1
            Layout.alignment: Qt.AlignVCenter
        }
        Kirigami.SearchField {
            id: search
            Layout.fillWidth: true
            Layout.minimumWidth: Kirigami.Units.gridUnit * 8
            placeholderText: "Search, or app: host: port: country:XX"
            Accessible.name: "Search connections. Use app:, host:, port: or country: and a value; terms are combined."
            onTextChanged: {
                if (header.model && text !== header.model.filterText) {
                    header.model.setFilterQuery(text)
                }
            }
            // Follow a query set from elsewhere, such as a map click.
            Connections {
                target: header.model
                function onFilterTextChanged() {
                    if (search.text !== header.model.filterText) {
                        search.text = header.model.filterText
                    }
                }
            }
        }
        Controls.Switch {
            id: grouped
            text: "Grouped"
            checked: header.model ? header.model.grouped : true
            // Must be the invokable, not the auto-generated `setGrouped`
            // property setter: only setGroupedMode brackets the Qt model
            // reset and rebuilds the grouped projection.
            onToggled: header.model.setGroupedMode(checked)
        }
    }

    // Wraps onto further lines at narrow widths instead of clipping.
    Flow {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        Controls.Switch {
            id: pendingOnly
            text: "Pending only"
            onToggled: header.model.setPendingOnly(checked)
        }
        Controls.Switch {
            id: deniedOnly
            text: "Denied only"
            onToggled: header.model.setDeniedOnly(checked)
        }
        Controls.Switch {
            id: fromNow
            readonly property bool on: header.model ? header.model.filterSinceMs > 0 : false
            text: on
                  ? "From now (since " + Qt.formatTime(new Date(header.model.filterSinceMs), "hh:mm:ss") + ")"
                  : "From now"
            checked: on
            onToggled: header.model.setFromNow(checked)
        }
    }

    Controls.Label {
        Layout.fillWidth: true
        visible: header.model ? header.model.filterSummary.length > 0 : false
        text: header.model ? "Filter: " + header.model.filterSummary : ""
        wrapMode: Text.WordWrap
        color: Kirigami.Theme.disabledTextColor
    }
}
