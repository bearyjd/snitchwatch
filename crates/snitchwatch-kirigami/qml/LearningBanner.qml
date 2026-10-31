// The learning window's banner (learning mode, phase L2). main.qml floats it
// over every page while a window runs; it is never shown otherwise.
//
// The sentence in the InlineMessage is fixed text (InlineMessage renders its
// text as markup, issue #51). Everything that varies, the time left, the
// count and the notices, is in PlainText labels: the notice text comes from
// the saved list's state. "Review" asks the host to open the review page
// (`reviewRequested`); "Stop learning" ends the window through
// SettingsController.stopLearning(), which restores the mode it replaced.
//
// Restyle (rounded style, batch F): nothing here needs a change. The
// Kirigami.InlineMessage is Kirigami-drawn; its two buttons are GuardedButton
// (already 44 px capsules); the details strip below is text only and takes no
// input.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: banner
    spacing: 0

    // The owner pressed Review: the host opens the review page.
    signal reviewRequested()

    // SettingsController, or a stand-in with learningActive, learningEndsMs,
    // programsSeen, learningIncomplete, droppedCount, learnedNotice,
    // learnedPath, refreshLearningStatus() and stopLearning().
    property var settings: null
    // Re-read by `ticker`, so the time left counts down.
    property double nowMs: Date.now()

    readonly property bool shown: banner.settings !== null
        && banner.settings.learningActive === true
    // Exposed for the headless probe (tests/learning_banner_qml.rs).
    property alias summary: summaryLabel
    property alias incompleteNotice: incompleteLabel
    property alias droppedNotice: droppedLabel
    property alias fileNotice: noticeLabel
    property alias whereLabel: whereLabel

    readonly property int minutesLeft: banner.settings === null ? 0
        : Math.max(0, Math.ceil((banner.settings.learningEndsMs - banner.nowMs) / 60000))
    readonly property string timeLeft: banner.minutesLeft >= 60
        ? Math.floor(banner.minutesLeft / 60) + " h " + (banner.minutesLeft % 60) + " min"
        : banner.minutesLeft + " min"
    readonly property int programs: banner.settings === null ? 0 : banner.settings.programsSeen

    visible: banner.shown

    Timer {
        id: ticker
        interval: 2000
        repeat: true
        running: banner.shown
        triggeredOnStart: true
        onTriggered: {
            banner.nowMs = Date.now();
            banner.settings.refreshLearningStatus();
        }
    }

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        visible: true
        type: Kirigami.MessageType.Information
        text: "Learning: new connections are allowed once and recorded."
        actions: [
            Kirigami.Action {
                text: "Stop learning"
                icon.name: "media-playback-stop"
                // A button that ignores a held key (GuardedButton); a narrow
                // bar's overflow menu uses the action itself.
                displayComponent: GuardedButton {
                    returnActivates: true
                    text: "Stop learning"
                    icon.name: "media-playback-stop"
                    onClicked: banner.settings.stopLearning()
                }
                onTriggered: banner.settings.stopLearning()
            },
            Kirigami.Action {
                text: "Review"
                icon.name: "view-list-details"
                displayComponent: GuardedButton {
                    returnActivates: true
                    text: "Review"
                    icon.name: "view-list-details"
                    onClicked: banner.reviewRequested()
                }
                onTriggered: banner.reviewRequested()
            }
        ]
    }

    Rectangle {
        Layout.fillWidth: true
        implicitHeight: details.implicitHeight + Kirigami.Units.smallSpacing * 2
        color: Kirigami.Theme.backgroundColor

        ColumnLayout {
            id: details
            anchors.fill: parent
            anchors.margins: Kirigami.Units.smallSpacing
            spacing: Kirigami.Units.smallSpacing

            SelectableText {
                id: summaryLabel
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                text: "Ends in " + banner.timeLeft + ". " + banner.programs
                    + (banner.programs === 1 ? " program" : " programs") + " seen."
            }
            SelectableText {
                id: incompleteLabel
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                visible: banner.settings !== null && banner.settings.learningIncomplete === true
                text: "Some connections were missed under heavy load, so this record is not complete."
            }
            SelectableText {
                id: droppedLabel
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                visible: banner.settings !== null && banner.settings.droppedCount > 0
                text: (banner.settings !== null ? banner.settings.droppedCount : 0)
                    + (banner.settings !== null && banner.settings.droppedCount === 1
                        ? " program was not recorded or was dropped to stay within the limits."
                        : " programs were not recorded or were dropped to stay within the limits.")
            }
            SelectableText {
                id: noticeLabel
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                visible: banner.settings !== null && banner.settings.learnedNotice !== ""
                text: banner.settings !== null ? banner.settings.learnedNotice : ""
            }
            SelectableText {
                id: whereLabel
                Layout.fillWidth: true
                wrapMode: Text.WrapAnywhere
                textFormat: Text.PlainText
                opacity: 0.7
                text: "Saved in " + (banner.settings !== null ? banner.settings.learnedPath : "")
            }
        }
    }
}
