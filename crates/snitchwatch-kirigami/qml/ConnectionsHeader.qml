// Connections page header, split out of ConnectionsPage.qml (monitor-tree
// step 1) with no behaviour change.
//
// The heading, search field and Grouped switch: one fixed-height row, the
// page's `titleDelegate`. The filter switches and the summary can wrap onto
// several lines, so they live in ConnectionsFilterBar.qml, above the list in
// the page content. Every control only forwards to the model: the token
// syntax and the matching are decided in Rust (connections/filter.rs).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

RowLayout {
    id: header
    // The owning ConnectionsPage supplies the model and the title.
    property var model
    property string title

    Layout.fillWidth: true
    spacing: Kirigami.Units.largeSpacing

    // Selectable (batch 8): a level-1 heading on one line, as before.
    SelectableText {
        text: header.title
        level: 1
        textFormat: Text.PlainText
        wrapMode: Text.NoWrap
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
