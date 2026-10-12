// Onboarding step: how Snitchwatch should treat a new connection when nobody
// is asked (LS redesign phase 1). "Allow and record" is preselected. Choosing
// "Deny and record" shows a warning and offers the existing recommended
// background-service rules page (`RecommendedRulesPage`, the curated-defaults
// opt-in) instead of a second copy of it. The choice is saved by
// `SettingsController.chooseSilentMode` when the user continues.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: step
    spacing: Kirigami.Units.largeSpacing

    // Injected by the caller (main.qml): the SettingsController.
    property var settings: null
    // The mode shown selected: the saved one when there is one.
    property string selected: step.settings && step.settings.silentMode !== ""
        ? step.settings.silentMode : SilentModes.recommended
    property alias denyWarningBox: denyWarning
    property alias continueButton: continueButton

    // Open the existing recommended-rules page (main.qml replaces the page).
    signal reviewRecommendedRules()
    // The choice was saved; the caller moves on.
    signal chosen()

    Kirigami.Heading {
        Layout.fillWidth: true
        level: 2
        wrapMode: Text.WordWrap
        text: "When a program wants the network"
    }

    Repeater {
        model: SilentModes.all
        delegate: ColumnLayout {
            required property var modelData
            Layout.fillWidth: true
            spacing: 0

            Controls.RadioButton {
                objectName: "silentMode-" + modelData.token
                Layout.fillWidth: true
                text: modelData.label
                checked: step.selected === modelData.token
                onClicked: step.selected = modelData.token
            }
            Controls.Label {
                Layout.fillWidth: true
                Layout.leftMargin: Kirigami.Units.gridUnit * 2
                wrapMode: Text.Wrap
                opacity: 0.7
                textFormat: Text.PlainText
                text: modelData.detail
            }
        }
    }

    Kirigami.InlineMessage {
        id: denyWarning
        objectName: "denyWarning"
        Layout.fillWidth: true
        visible: step.selected === "deny"
        type: Kirigami.MessageType.Warning
        text: SilentModes.denyWarning
        actions: [
            Kirigami.Action {
                text: "Review recommended background-service rules"
                icon.name: "checkmark"
                enabled: step.settings !== null
                onTriggered: {
                    step.settings.chooseSilentMode(step.selected);
                    step.reviewRecommendedRules();
                }
            }
        ]
    }

    Controls.Label {
        objectName: "notRunningNote"
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        opacity: 0.7
        textFormat: Text.PlainText
        text: SilentModes.notRunningNote
    }

    Controls.Button {
        id: continueButton
        objectName: "silentModeContinue"
        Layout.alignment: Qt.AlignRight
        text: "Continue"
        icon.name: "go-next"
        enabled: step.settings !== null
        onClicked: {
            step.settings.chooseSilentMode(step.selected);
            step.chosen();
        }
    }
}
