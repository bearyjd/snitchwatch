// Rule import preview (roadmap P2.7): what a rules file would install, before
// anything is sent.
//
// The bridge checked every rule and compared it with the firewall's rules
// (`RulesIoController.previewJson`, grouped by `rules::io::group`): new rules,
// replacements of a same-name rule (the firewall overwrites it in place),
// unchanged rules, and refused ones with each reason. Nothing is deleted:
// rules missing from the file stay as they are.
//
// Default ticks and their reasons come from the bridge: adds and replaces
// start ticked, except a replace that loosens or changes a blocking rule or
// widens an allow, an allow that overrides other rules, and an allow that
// applies to every app, which start unticked with a reason. A replace also
// shows the rule it overwrites. At most 2,000 changes are applied at once
// (the bridge's `MAX_APPLY_RULES`).
//
// Every label is plain text: names, conditions and reasons come from the file
// or the firewall service. Checkboxes carry no text; the name sits in a
// PlainText label beside each.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

SizedOverlaySheet {
    id: sheet
    title: "Import rules"
    preferredWidth: Kirigami.Units.gridUnit * 36

    property RulesIoController controller
    // `rules::io::PreviewView`.
    property var preview: ({ add: [], replace: [], unchanged: [], refused: [] })
    // Rule name -> ticked.
    property var ticks: ({})
    // Rule name -> outcome text, while and after applying.
    property var results: ({})
    property bool showUnchanged: false
    readonly property int tickedCount: sheet.countTicked()
    // The bridge refuses more in one apply (`rules_import::MAX_APPLY_RULES`).
    readonly property int maxApply: 2000

    function load() {
        const json = sheet.controller ? sheet.controller.previewJson : "";
        sheet.preview = json ? JSON.parse(json)
                             : { add: [], replace: [], unchanged: [], refused: [] };
        const ticks = {};
        for (const row of sheet.preview.add.concat(sheet.preview.replace)) {
            ticks[row.name] = row.ticked;
        }
        sheet.ticks = ticks;
        sheet.results = ({});
        sheet.showUnchanged = false;
    }

    function setTicked(name, on) {
        const ticks = Object.assign({}, sheet.ticks);
        ticks[name] = on;
        sheet.ticks = ticks;
    }

    function tickedNames() {
        return Object.keys(sheet.ticks).filter(function (name) { return sheet.ticks[name]; });
    }

    function countTicked() {
        return sheet.tickedNames().length;
    }

    function applyTicked() {
        if (sheet.controller) {
            sheet.controller.apply(JSON.stringify(sheet.tickedNames()));
        }
    }

    Connections {
        target: sheet.controller
        function onPreviewReady() {
            sheet.load();
            sheet.open();
        }
        function onResultsJsonChanged() {
            const json = sheet.controller.resultsJson;
            sheet.results = json ? JSON.parse(json) : ({});
        }
    }

    // One add or replace: a tick, the name, why it starts unticked, its
    // badges, what it installs, and its outcome once applied.
    Component {
        id: changeRow

        ColumnLayout {
            id: change
            required property var modelData
            readonly property var row: modelData
            Layout.fillWidth: true
            spacing: 0

            RowLayout {
                Layout.fillWidth: true
                Controls.CheckBox {
                    objectName: "importTick"
                    checked: !!sheet.ticks[change.row.name]
                    enabled: !!sheet.controller && !sheet.controller.applying
                             && !sheet.controller.applied
                    onToggled: sheet.setTicked(change.row.name, checked)
                }
                Controls.Label {
                    objectName: "importRowName"
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    text: change.row.displayName
                    font.bold: true
                    elide: Text.ElideMiddle
                }
            }
            Controls.Label {
                objectName: "importCaution"
                visible: change.row.caution.length > 0
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: change.row.caution
                color: Kirigami.Theme.neutralTextColor
                wrapMode: Text.Wrap
            }
            Repeater {
                model: change.row.badges
                delegate: Controls.Label {
                    required property string modelData
                    objectName: "importBadge"
                    textFormat: Text.PlainText
                    text: modelData
                    color: Kirigami.Theme.neutralTextColor
                    font.bold: true
                }
            }
            Repeater {
                model: change.row.details
                delegate: Controls.Label {
                    required property string modelData
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    text: modelData
                    font: Kirigami.Theme.smallFont
                    wrapMode: Text.Wrap
                }
            }
            Controls.Label {
                objectName: "importPreviousHeading"
                visible: change.row.previous.length > 0
                textFormat: Text.PlainText
                text: "It replaces this rule:"
                font: Kirigami.Theme.smallFont
                opacity: 0.8
            }
            Repeater {
                model: change.row.previous
                delegate: Controls.Label {
                    required property string modelData
                    objectName: "importPrevious"
                    Layout.fillWidth: true
                    Layout.leftMargin: Kirigami.Units.largeSpacing
                    textFormat: Text.PlainText
                    text: modelData
                    font: Kirigami.Theme.smallFont
                    opacity: 0.8
                    wrapMode: Text.Wrap
                }
            }
            Controls.Label {
                objectName: "importResult"
                visible: sheet.results[change.row.name] !== undefined
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: sheet.results[change.row.name] || ""
                wrapMode: Text.Wrap
            }
        }
    }

    // An unchanged or refused rule: the name and, when refused, each reason.
    Component {
        id: plainRow

        ColumnLayout {
            id: plain
            required property var modelData
            readonly property var row: modelData
            Layout.fillWidth: true
            spacing: 0

            Controls.Label {
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: plain.row.displayName
                elide: Text.ElideMiddle
            }
            Repeater {
                model: plain.row.problems
                delegate: Controls.Label {
                    required property string modelData
                    objectName: "importProblem"
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    text: modelData
                    color: Kirigami.Theme.negativeTextColor
                    font: Kirigami.Theme.smallFont
                    wrapMode: Text.Wrap
                }
            }
        }
    }

    ColumnLayout {
        Layout.preferredWidth: sheet.preferredWidth
        spacing: Kirigami.Units.largeSpacing

        Controls.Label {
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: "Nothing is sent until you apply. Rules missing from the file stay as they are."
            wrapMode: Text.Wrap
        }

        Controls.Label {
            objectName: "importSectionAdd"
            visible: sheet.preview.add.length > 0
            textFormat: Text.PlainText
            text: "New rules (" + sheet.preview.add.length + ")"
            font.bold: true
        }
        Repeater {
            model: sheet.preview.add
            delegate: changeRow
        }

        Controls.Label {
            objectName: "importSectionReplace"
            visible: sheet.preview.replace.length > 0
            textFormat: Text.PlainText
            text: "Replace a rule with the same name (" + sheet.preview.replace.length + ")"
            font.bold: true
        }
        Repeater {
            model: sheet.preview.replace
            delegate: changeRow
        }

        Controls.Label {
            objectName: "importSectionRefused"
            visible: sheet.preview.refused.length > 0
            textFormat: Text.PlainText
            text: "Not imported (" + sheet.preview.refused.length + ")"
            font.bold: true
        }
        Repeater {
            model: sheet.preview.refused
            delegate: plainRow
        }

        Controls.Button {
            objectName: "importShowUnchanged"
            visible: sheet.preview.unchanged.length > 0
            flat: true
            text: sheet.showUnchanged ? "Hide unchanged rules" : "Show unchanged rules"
            onClicked: sheet.showUnchanged = !sheet.showUnchanged
        }
        Controls.Label {
            objectName: "importSectionUnchanged"
            visible: sheet.showUnchanged && sheet.preview.unchanged.length > 0
            textFormat: Text.PlainText
            text: "Already the same (" + sheet.preview.unchanged.length + ")"
            font.bold: true
        }
        Repeater {
            model: sheet.showUnchanged ? sheet.preview.unchanged : []
            delegate: plainRow
        }

        Kirigami.Separator {
            Layout.fillWidth: true
        }

        Controls.Label {
            objectName: "importStatus"
            visible: text.length > 0
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: sheet.controller ? sheet.controller.statusText : ""
            wrapMode: Text.Wrap
        }

        Controls.Label {
            objectName: "importTooMany"
            visible: sheet.tickedCount > sheet.maxApply
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: "Apply at most 2,000 changes at once. Untick some, apply, then import the "
                + "file again for the rest."
            color: Kirigami.Theme.neutralTextColor
            wrapMode: Text.Wrap
        }

        Controls.Button {
            objectName: "importApply"
            Layout.fillWidth: true
            visible: !sheet.controller || !sheet.controller.applied
            enabled: sheet.tickedCount > 0 && sheet.tickedCount <= sheet.maxApply
                     && !!sheet.controller && !sheet.controller.busy
            text: sheet.tickedCount === 1 ? "Apply 1 change" : "Apply " + sheet.tickedCount + " changes"
            icon.name: "dialog-ok-apply"
            onClicked: sheet.applyTicked()
        }
    }
}
