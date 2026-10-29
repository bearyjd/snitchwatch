// The batch's progress (learning mode, phase L4b): one status per rule, the
// counts line and Cancel while it runs. Labels carry program names, so they
// are PlainText.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: list
    spacing: Kirigami.Units.smallSpacing

    // LearningReviewController, or a stand-in with progressJson, applying and
    // cancelApply().
    property var controller: null
    readonly property var progress: {
        if (!list.controller || list.controller.progressJson === "") {
            return null;
        }
        try {
            return JSON.parse(list.controller.progressJson);
        } catch (e) {
            return null;
        }
    }
    property alias summaryLabel: summaryLabel

    visible: list.progress !== null || (list.controller !== null && list.controller.applying === true)

    Controls.Label {
        id: summaryLabel
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        font.bold: true
        visible: list.progress !== null
        text: list.progress === null ? "" : list.progress.summary
    }
    GuardedButton {
        visible: list.controller !== null && list.controller.applying === true
        text: "Cancel"
        onClicked: list.controller.cancelApply()
    }
    // One status per rule; hundreds are possible, so a ListView of fixed
    // height makes only the visible ones.
    ListView {
        objectName: "learningProgressRows"
        Layout.fillWidth: true
        Layout.preferredHeight: Math.min(contentHeight, Kirigami.Units.gridUnit * 16)
        clip: true
        interactive: contentHeight > height
        model: list.progress === null ? [] : list.progress.rows
        Controls.ScrollBar.vertical: Controls.ScrollBar {}
        delegate: ColumnLayout {
            required property var modelData
            width: ListView.view.width - Kirigami.Units.gridUnit
            spacing: 0
            Controls.Label {
                Layout.fillWidth: true
                wrapMode: Text.WrapAnywhere
                textFormat: Text.PlainText
                text: modelData.label
            }
            Controls.Label {
                Layout.fillWidth: true
                Layout.leftMargin: Kirigami.Units.gridUnit
                wrapMode: Text.WrapAnywhere
                textFormat: Text.PlainText
                opacity: 0.8
                text: modelData.status
            }
        }
    }
}
