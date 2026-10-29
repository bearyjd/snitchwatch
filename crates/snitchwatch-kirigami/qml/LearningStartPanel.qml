// "Start learning" (learning mode, phase L4b). Learning can only be switched
// on from here, and only now that recording and the review both exist.
// Everything on it is fixed text except the error sentence, which is shown in
// a PlainText label. Starting needs the owner to tick that everything is
// allowed once while it runs.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: panel
    spacing: Kirigami.Units.smallSpacing

    // SettingsController, or a stand-in with learningActive,
    // learningStartError, startLearning(hours) and stopLearning().
    property var settings: null

    readonly property bool running: panel.settings !== null && panel.settings.learningActive === true
    // The choices, in hours. 24 hours is the default; 7 days is the most.
    readonly property var hoursChoices: [1, 6, 24, 72, 168]
    readonly property int chosenHours: panel.hoursChoices[lengthBox.currentIndex]
    property alias lengthChoice: lengthBox
    property alias startButton: startButton
    property alias stopButton: stopButton
    property alias errorLabel: errorLabel

    property alias understood: understoodBox
    property alias warning: warningLabel

    Controls.Label {
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        text: "Learning records which programs connect where. No rule is made until you tick one below. It ends by itself, or when you press Stop."
    }

    Flow {
        objectName: "learningStartFlow"
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        Controls.Label {
            height: lengthBox.height
            verticalAlignment: Text.AlignVCenter
            textFormat: Text.PlainText
            text: "For"
        }
        Controls.ComboBox {
            id: lengthBox
            Accessible.name: "For how long: " + lengthBox.displayText
            enabled: !panel.running
            model: ["1 hour", "6 hours", "24 hours (default)", "3 days", "7 days (the longest)"]
            currentIndex: 2
        }
    }

    Controls.Label {
        id: warningLabel
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        font.bold: true
        visible: !panel.running
        text: "Everything is allowed once for the whole window, including software you did not mean to run. Deny rules and blocklists still apply."
    }
    RowLayout {
        Layout.fillWidth: true
        visible: !panel.running
        Controls.CheckBox {
            id: understoodBox
            objectName: "learningUnderstood"
            Accessible.name: "I understand that everything is allowed once while learning runs"
        }
        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            text: "I understand that everything is allowed once while learning runs."
            TapHandler {
                onTapped: understoodBox.toggle()
            }
        }
    }

    Flow {
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        GuardedButton {
            id: startButton
            text: "Start learning"
            icon.name: "media-playback-start"
            enabled: panel.settings !== null && !panel.running && understoodBox.checked
            onClicked: {
                panel.settings.startLearning(panel.chosenHours);
                understoodBox.checked = false;
            }
        }
        GuardedButton {
            id: stopButton
            text: "Stop learning"
            icon.name: "media-playback-stop"
            visible: panel.running
            onClicked: panel.settings.stopLearning()
        }
    }

    Controls.Label {
        id: errorLabel
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        visible: panel.settings !== null && panel.settings.learningStartError !== ""
        text: panel.settings !== null ? panel.settings.learningStartError : ""
    }
}
