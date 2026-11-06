// The exception-first default's controls (clarity 0c): "Showing N of M ·
// Show all", the collapsed "Background and system" group with its one-line
// summary (never hidden: Expand shows its rows again), the programs new this
// session, and the one-time baseline notice. Which rows are handled is decided
// in Rust (connections/attention.rs); one model bool, filterNeedsAttention,
// sits under Show all, Expand/Collapse and the filter bar's Full list switch.
import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Controls.impl as Impl
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: bar
    property var model

    readonly property bool collapsing: model ? model.filterNeedsAttention : false
    readonly property int background: model ? model.backgroundCount : 0
    readonly property int programs: model ? model.backgroundPrograms : 0
    readonly property bool available: model ? model.backgroundAvailable : false
    readonly property int newPrograms: model ? model.newProgramCount : 0
    readonly property string notice: model ? model.baselineNotice : ""

    function plural(n, one, many) { return n + " " + (n === 1 ? one : many) }

    spacing: Kirigami.Units.smallSpacing

    RowLayout {
        objectName: "showingRow"
        visible: bar.collapsing && bar.background > 0
        Layout.fillWidth: true
        SelectableText {
            objectName: "showingText"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: bar.model
                  ? "Showing " + bar.model.matchingCount + " of " + bar.model.totalCount + " connections"
                  : ""
        }
        Controls.Button {
            id: showAllButton
            objectName: "showAllButton"
            // Restyled in place (type name stays `Button` for the focus tests).
            implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
            horizontalPadding: Theme.controlPaddingH
            background: RoundedSurface { control: showAllButton }
            // org.kde.desktop draws a Button's label inside the background
            // replaced above; without its own label it would render blank.
            contentItem: Impl.IconLabel {
                objectName: "inPlaceButtonLabel"
                spacing: showAllButton.spacing
                mirrored: showAllButton.mirrored
                display: showAllButton.display
                icon: showAllButton.icon
                text: showAllButton.text
                font: showAllButton.font
                color: showAllButton.palette.buttonText
            }
            text: "Show all"
            onClicked: bar.model.setNeedsAttention(false)
        }
    }

    RowLayout {
        objectName: "backgroundGroup"
        // Collapsed: only with rows in it. Expanded, it is not counted (no
        // work while the full list shows), so it stays while rules allow it.
        visible: bar.collapsing ? bar.background > 0 : bar.available
        Layout.fillWidth: true
        Kirigami.Icon {
            source: bar.collapsing ? "go-next" : "go-down"
            implicitWidth: Kirigami.Units.iconSizes.small
            implicitHeight: Kirigami.Units.iconSizes.small
        }
        SelectableText {
            objectName: "backgroundSummary"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: bar.collapsing
                  ? "Background and system: "
                    + bar.plural(bar.background, "connection", "connections") + " from "
                    + bar.plural(bar.programs, "program", "programs")
                    + ", allowed by recommended rules, collapsed here"
                  : "Background and system: connections allowed by recommended rules are "
                    + "in the list, subject to the other filters"
        }
        Controls.Button {
            id: backgroundToggle
            objectName: "backgroundToggle"
            // Restyled in place (type name stays `Button` for the focus tests).
            implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
            horizontalPadding: Theme.controlPaddingH
            background: RoundedSurface { control: backgroundToggle }
            // org.kde.desktop draws a Button's label inside the background
            // replaced above; without its own label it would render blank.
            contentItem: Impl.IconLabel {
                objectName: "inPlaceButtonLabel"
                spacing: backgroundToggle.spacing
                mirrored: backgroundToggle.mirrored
                display: backgroundToggle.display
                icon: backgroundToggle.icon
                text: backgroundToggle.text
                font: backgroundToggle.font
                color: backgroundToggle.palette.buttonText
            }
            text: bar.collapsing ? "Expand" : "Collapse"
            onClicked: bar.model.setNeedsAttention(!bar.collapsing)
        }
    }

    RowLayout {
        objectName: "newProgramsRow"
        visible: bar.newPrograms > 0
        Layout.fillWidth: true
        SelectableText {
            objectName: "newProgramsText"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: bar.plural(bar.newPrograms, "program", "programs")
                  + " first seen this session: always shown until you mark them as seen"
        }
        Controls.Button {
            id: markSeenButton
            objectName: "markSeenButton"
            // Restyled in place (type name stays `Button` for the focus tests).
            implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
            horizontalPadding: Theme.controlPaddingH
            background: RoundedSurface { control: markSeenButton }
            // org.kde.desktop draws a Button's label inside the background
            // replaced above; without its own label it would render blank.
            contentItem: Impl.IconLabel {
                objectName: "inPlaceButtonLabel"
                spacing: markSeenButton.spacing
                mirrored: markSeenButton.mirrored
                display: markSeenButton.display
                icon: markSeenButton.icon
                text: markSeenButton.text
                font: markSeenButton.font
                color: markSeenButton.palette.buttonText
            }
            text: "Mark as seen"
            onClicked: bar.model.markNewProgramsSeen()
        }
    }

    RowLayout {
        objectName: "baselineNotice"
        visible: bar.notice.length > 0
        Layout.fillWidth: true
        SelectableText {
            objectName: "baselineText"
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: bar.notice
        }
        Controls.Button {
            id: dismissBaseline
            objectName: "dismissBaseline"
            // Restyled in place (type name stays `Button` for the focus tests).
            implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
            horizontalPadding: Theme.controlPaddingH
            background: RoundedSurface { control: dismissBaseline }
            // org.kde.desktop draws a Button's label inside the background
            // replaced above; without its own label it would render blank.
            contentItem: Impl.IconLabel {
                objectName: "inPlaceButtonLabel"
                spacing: dismissBaseline.spacing
                mirrored: dismissBaseline.mirrored
                display: dismissBaseline.display
                icon: dismissBaseline.icon
                text: dismissBaseline.text
                font: dismissBaseline.font
                color: dismissBaseline.palette.buttonText
            }
            text: "Dismiss"
            onClicked: bar.model.dismissBaselineNotice()
        }
    }
}
