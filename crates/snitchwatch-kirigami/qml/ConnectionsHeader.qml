// Connections page header, split out of ConnectionsPage.qml (monitor-tree
// step 1) with no behaviour change.
//
// Search + pending-only filter live in the page header so they stay visible
// while the list scrolls.
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

    Kirigami.Heading {
        text: header.title
        level: 1
        Layout.alignment: Qt.AlignVCenter
    }
    Kirigami.SearchField {
        id: search
        Layout.fillWidth: true
        placeholderText: "Filter by process, host, port…"
        onTextChanged: header.model.setFilterQuery(text)
    }
    Controls.Switch {
        id: pendingOnly
        text: "Pending only"
        onToggled: header.model.setPendingOnly(checked)
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
