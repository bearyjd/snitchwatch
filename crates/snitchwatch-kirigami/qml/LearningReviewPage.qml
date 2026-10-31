// "Make rules from what I saw" (learning mode, phase L4b): the Start learning
// control, what the record lacks, and one row per recorded program with its
// choices. NOTHING is ticked by default; "Select suggested" is an explicit act
// that ticks only narrow rules. "Review the ticked choices" opens the summary
// that repeats every rule; only its Apply button sends anything.
//
// The rows are JSON from the controller (parsed here, as the scanner page does
// with its report). Every recorded string (a name, a path, a host, a flag) is
// hostile text as far as markup goes: every label is PlainText. The page states
// the limits of learning mode; it says nothing about what a program is.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Kirigami.Page {
    id: page
    title: "Make rules from what I saw"

    // LearningReviewController and SettingsController, or stand-ins.
    property var controller: null
    property var settings: null
    // The first click on "Delete the recorded list" only asks.
    property bool confirmingDelete: false

    function parsed(text, fallback) {
        try {
            return JSON.parse(text);
        } catch (e) {
            return fallback;
        }
    }

    readonly property var rows: page.controller ? page.parsed(page.controller.rowsJson, []) : []
    readonly property var ticks: page.controller ? page.parsed(page.controller.ticksJson, {}) : ({})
    readonly property var notices: page.controller ? page.parsed(page.controller.noticesJson, {}) : ({})
    readonly property bool learning: page.settings !== null && page.settings.learningActive === true
    readonly property bool applying: page.controller !== null && page.controller.applying === true
    readonly property int tickCount: page.controller ? page.controller.tickCount : 0
    property alias rowList: rowList
    property alias summaryDialog: summaryDialog

    // Reading the saved list is the page's first act, not the controller's.
    // A summary frozen when the page was last closed is not shown again.
    Component.onCompleted: if (page.controller) {
        page.controller.discardSummary();
        page.controller.refresh();
    }
    Component.onDestruction: if (page.controller) page.controller.discardSummary()
    onLearningChanged: if (page.learning) page.confirmingDelete = false
    onApplyingChanged: if (page.applying) page.confirmingDelete = false

    ListView {
        id: rowList
        anchors.fill: parent
        clip: true
        spacing: Kirigami.Units.smallSpacing
        model: page.rows
        Controls.ScrollBar.vertical: Controls.ScrollBar {}

        header: ColumnLayout {
            width: rowList.width
            spacing: Kirigami.Units.largeSpacing

            LearningStartPanel {
                objectName: "learningStartPanel"
                Layout.fillWidth: true
                settings: page.settings
            }
            LearningLimitsList {
                objectName: "learningLimits"
                Layout.fillWidth: true
            }
            ColumnLayout {
                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing

                SelectableText {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    text: "Programs recorded: " + (page.notices.programs === undefined ? 0 : page.notices.programs)
                        + ". Saved in " + (page.settings ? page.settings.learnedPath : "") + "."
                }
                SelectableText {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    visible: page.notices.incomplete === true
                    text: "Some connections were missed under heavy load, so this record is not complete."
                }
                SelectableText {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    visible: page.notices.dropped > 0
                    text: "Programs not recorded or dropped to stay within the limits: " + page.notices.dropped
                        + " (" + page.notices.droppedFlagged + " of the last ones listed carried a flag)."
                }
                SelectableText {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    font.bold: true
                    visible: page.notices.classAFull === true
                    text: "The group of programs with flags is full, so new programs with flags are being refused."
                }
                SelectableText {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    visible: page.notices.dirsFull > 0
                    text: "Folders that hold more programs than an entry lists: " + page.notices.dirsFull
                        + ". The rest are only counted (" + page.notices.dirUnlistedConnections + " connections)."
                }
                SelectableText {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    visible: page.notices.rulesRead === false
                    text: "The rules list could not be read, so programs you already have rules for are not hidden here. Identical rules are skipped when applying."
                }
                SelectableText {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    visible: page.controller !== null && page.controller.fileNotice !== ""
                    text: page.controller ? page.controller.fileNotice : ""
                }
            }
            Flow {
                objectName: "learningButtonFlow"
                Layout.fillWidth: true
                spacing: Kirigami.Units.smallSpacing

                Controls.Button {
                    objectName: "learningSuggestButton"
                    text: "Select suggested"
                    enabled: !page.applying
                    onClicked: page.controller.selectSuggested()
                }
                Controls.Button {
                    text: "Untick all"
                    enabled: !page.applying && page.tickCount > 0
                    onClicked: page.controller.untickAll()
                }
                Controls.Button {
                    text: "Refresh"
                    enabled: !page.applying
                    onClicked: page.controller.refresh()
                }
                Controls.Button {
                    objectName: "learningClearButton"
                    text: "Delete the recorded list"
                    enabled: !page.learning && !page.applying
                    onClicked: page.confirmingDelete = true
                }
                Controls.Button {
                    objectName: "learningReviewButton"
                    text: "Review the ticked choices"
                    highlighted: true
                    enabled: !page.applying && page.tickCount > 0
                    onClicked: page.controller.prepareSummary()
                }
            }
            ColumnLayout {
                objectName: "learningDeleteConfirm"
                Layout.fillWidth: true
                visible: page.confirmingDelete && !page.learning && !page.applying
                spacing: Kirigami.Units.smallSpacing

                SelectableText {
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    font.bold: true
                    text: "This deletes everything recorded so far, up to 7 days of it. Rules already made stay. It cannot be undone."
                }
                Flow {
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing
                    GuardedButton {
                        objectName: "learningDeleteYes"
                        text: "Yes, delete the recorded list"
                        onClicked: {
                            page.confirmingDelete = false;
                            page.controller.clearData();
                        }
                    }
                    Controls.Button {
                        objectName: "learningDeleteNo"
                        text: "Keep it"
                        onClicked: page.confirmingDelete = false
                    }
                }
            }
            SelectableText {
                objectName: "learningStatusLabel"
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                visible: text !== ""
                text: page.controller ? page.controller.statusText : ""
            }
            LearningProgressList {
                Layout.fillWidth: true
                controller: page.controller
            }
            SelectableText {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                visible: page.controller !== null && page.controller.loaded === true && page.rows.length === 0
                text: "Nothing has been recorded."
            }
        }

        delegate: LearningReviewRow {
            required property var modelData
            width: ListView.view.width
            info: modelData
            selected: page.ticks[modelData.key] === undefined ? "" : page.ticks[modelData.key]
            controller: page.controller
            locked: page.applying
        }
    }

    LearningSummaryDialog {
        id: summaryDialog
        controller: page.controller
    }
}
