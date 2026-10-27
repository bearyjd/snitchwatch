// The exception-first default's controls (clarity 0c): "Showing N of M ·
// Show all", the collapsed "Background and system" group with its one-line
// summary (never hidden: Expand shows its rows again), the programs new this
// session, and the one-time baseline notice. Which rows are handled is decided
// in Rust (connections/attention.rs); one model bool, filterNeedsAttention,
// sits under Show all, Expand/Collapse and the filter bar's Full list switch.
import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: bar
    property var model

    readonly property bool collapsing: model ? model.filterNeedsAttention : false
    readonly property int background: model ? model.backgroundCount : 0
    readonly property int programs: model ? model.backgroundPrograms : 0
    readonly property int newPrograms: model ? model.newProgramCount : 0
    readonly property string notice: model ? model.baselineNotice : ""

    function plural(n, one, many) { return n + " " + (n === 1 ? one : many) }

    spacing: Kirigami.Units.smallSpacing

    RowLayout {
        objectName: "showingRow"
        visible: bar.collapsing && bar.background > 0
        Layout.fillWidth: true
        Controls.Label {
            objectName: "showingText"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: bar.model
                  ? "Showing " + bar.model.matchingCount + " of " + bar.model.totalCount + " connections"
                  : ""
        }
        Controls.Button {
            objectName: "showAllButton"
            text: "Show all"
            onClicked: bar.model.setNeedsAttention(false)
        }
    }

    RowLayout {
        objectName: "backgroundGroup"
        visible: bar.background > 0
        Layout.fillWidth: true
        Kirigami.Icon {
            source: bar.collapsing ? "go-next" : "go-down"
            implicitWidth: Kirigami.Units.iconSizes.small
            implicitHeight: Kirigami.Units.iconSizes.small
        }
        Controls.Label {
            objectName: "backgroundSummary"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: "Background and system: "
                  + bar.plural(bar.background, "connection", "connections") + " from "
                  + bar.plural(bar.programs, "program", "programs")
                  + (bar.collapsing
                     ? ", allowed by recommended rules, collapsed here"
                     : ", allowed by recommended rules, shown in the list")
        }
        Controls.Button {
            objectName: "backgroundToggle"
            text: bar.collapsing ? "Expand" : "Collapse"
            onClicked: bar.model.setNeedsAttention(!bar.collapsing)
        }
    }

    RowLayout {
        objectName: "newProgramsRow"
        visible: bar.newPrograms > 0
        Layout.fillWidth: true
        Controls.Label {
            objectName: "newProgramsText"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: bar.plural(bar.newPrograms, "program", "programs")
                  + " first seen this session: always shown until you mark them as seen"
        }
        Controls.Button {
            objectName: "markSeenButton"
            text: "Mark as seen"
            onClicked: bar.model.markNewProgramsSeen()
        }
    }

    RowLayout {
        objectName: "baselineNotice"
        visible: bar.notice.length > 0
        Layout.fillWidth: true
        Controls.Label {
            objectName: "baselineText"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: bar.notice
        }
        Controls.Button {
            objectName: "dismissBaseline"
            text: "Dismiss"
            onClicked: bar.model.dismissBaselineNotice()
        }
    }
}
