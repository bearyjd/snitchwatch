// What sits above the Connections list, in the page content where its height
// is laid out rather than overflowed: the wrapping filter switches and summary,
// then the header rule buttons' commands, confirm and outcome bar. Split out of
// ConnectionsPage.qml to keep it under its line budget.
import QtQuick
import QtQuick.Layouts

ColumnLayout {
    id: bar

    property var model
    property alias filterBar: filters
    property alias ruleActions: actions
    spacing: 0

    ConnectionsFilterBar {
        id: filters
        Layout.fillWidth: true
        model: bar.model
    }
    LevelRuleActions {
        id: actions
        Layout.fillWidth: true
        model: bar.model
    }
}
