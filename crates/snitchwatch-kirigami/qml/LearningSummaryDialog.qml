// The summary before anything is sent (learning mode, phase L4b): EVERY rule
// "Apply" will send, one plain sentence each, and the extra confirmations that
// blocks of system programs and "any destination" for programs that run other
// programs need. The Apply button stays disabled until each one is ticked; the
// controller refuses an unconfirmed apply as well. All sentences carry program
// names and paths, so every label is PlainText; the dialog title and the
// buttons are fixed text.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Dialog {
    id: dialog
    objectName: "learningSummaryDialog"

    // LearningReviewController, or a stand-in with summaryJson,
    // discardSummary(), setExtraConfirmed(i, on) and apply().
    property var controller: null
    readonly property var summary: {
        if (!dialog.controller) {
            return null;
        }
        try {
            return JSON.parse(dialog.controller.summaryJson);
        } catch (e) {
            return null;
        }
    }
    readonly property bool allConfirmed: dialog.summary !== null
        && dialog.summary.confirmed.every(given => given === true)
    property alias applyButton: applyButton
    property alias lineList: lineRepeater

    parent: Controls.Overlay.overlay
    anchors.centerIn: parent
    width: Math.min(parent ? parent.width - 32 : 640, 640)
    height: Math.min(parent ? parent.height - 32 : 520, 560)
    modal: true
    title: "These rules will be added"
    closePolicy: Controls.Popup.CloseOnEscape
    visible: dialog.summary !== null
    onClosed: if (dialog.controller) dialog.controller.discardSummary()

    header: Kirigami.Heading {
        level: 2
        textFormat: Text.PlainText
        text: "These rules will be added"
        padding: Kirigami.Units.largeSpacing
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        // The rule lines can be many (hundreds): a ListView makes only the
        // visible ones.
        ListView {
            id: lineRepeater
            objectName: "learningSummaryLines"
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.minimumHeight: Kirigami.Units.gridUnit * 6
            clip: true
            spacing: Kirigami.Units.smallSpacing
            model: dialog.summary === null ? [] : dialog.summary.lines
            Controls.ScrollBar.vertical: Controls.ScrollBar {}
            delegate: Controls.Label {
                required property string modelData
                width: ListView.view.width - Kirigami.Units.gridUnit
                wrapMode: Text.WrapAnywhere
                textFormat: Text.PlainText
                text: modelData
            }
        }
        Controls.Label {
            Layout.fillWidth: true
            visible: dialog.summary !== null && dialog.summary.droppedNames > 0
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            text: dialog.summary === null ? ""
                : dialog.summary.droppedNames + " repeated rule names are added once."
        }
        // The confirmations: few, but scrollable and capped in height.
        ListView {
            id: extraList
            objectName: "learningSummaryExtras"
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(contentHeight, Kirigami.Units.gridUnit * 8)
            visible: count > 0
            clip: true
            spacing: Kirigami.Units.smallSpacing
            model: dialog.summary === null ? [] : dialog.summary.extras
            Controls.ScrollBar.vertical: Controls.ScrollBar {}
            delegate: RowLayout {
                required property int index
                required property string modelData
                width: ListView.view.width - Kirigami.Units.gridUnit
                Controls.CheckBox {
                    id: extraBox
                    objectName: "extra-" + index
                    checked: dialog.summary !== null && dialog.summary.confirmed[index] === true
                    Accessible.name: "Confirm: " + modelData
                    onToggled: {
                        dialog.controller.setExtraConfirmed(index, checked);
                        checked = Qt.binding(() => dialog.summary !== null
                            && dialog.summary.confirmed[index] === true);
                    }
                }
                Controls.Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WrapAnywhere
                    textFormat: Text.PlainText
                    font.bold: true
                    text: modelData
                    TapHandler {
                        onTapped: {
                            extraBox.toggle();
                            extraBox.toggled();
                        }
                    }
                }
            }
        }
        // A refusal or error from Apply, here where the owner is looking.
        Controls.Label {
            id: statusLabel
            objectName: "learningSummaryStatus"
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            visible: text !== ""
            text: dialog.controller ? dialog.controller.statusText : ""
        }

        RowLayout {
            Layout.alignment: Qt.AlignRight
            Controls.Button {
                text: "Go back"
                onClicked: dialog.close()
            }
            Controls.Button {
                id: applyButton
                objectName: "learningApplyButton"
                enabled: dialog.allConfirmed
                Accessible.name: "Apply " + (dialog.summary === null ? 0 : dialog.summary.count) + " rules"
                contentItem: Controls.Label {
                    textFormat: Text.PlainText
                    horizontalAlignment: Text.AlignHCenter
                    opacity: applyButton.enabled ? 1.0 : 0.5
                    text: "Apply " + (dialog.summary === null ? 0 : dialog.summary.count) + " rules"
                }
                onClicked: dialog.controller.apply()
            }
        }
    }
}
