// Rule editor (roadmap P2.1): write a new rule, or change one Snitchwatch
// may change.
//
// Conditions are ANDed: a connection matches when every condition does. Each
// condition offers only the match kinds the bridge accepts for its operand
// (`rules::editor::operands`). Every change is checked as the bridge will
// check it (`RuleEditorController.check`): problems block saving; warnings
// say what the rule does that may surprise; cautions say what replacing the
// old rule loosens, and saving over them takes a second click.
//
// The bridge checks again, refuses read-only rules, and answers the save.
// The sheet stays open until the firewall confirms it, and shows why when it
// doesn't. Renaming saves the rule under the new name first, then removes
// the old one.
//
// Every label is plain text: names, values and reasons can come from the
// firewall service. Checkboxes carry no text; their label sits beside them.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

SizedOverlaySheet {
    id: sheet
    title: sheet.controller && sheet.controller.editingName.length > 0 ? "Edit rule" : "New rule"
    preferredWidth: Kirigami.Units.gridUnit * 34

    property RuleEditorController controller
    // `rules::editor::RuleDraft`.
    property var draft: ({ name: "", description: "", enabled: true, action: "deny",
                           duration: "always", precedence: false, nolog: false,
                           conditions: [] })
    readonly property var checked: sheet.controller && sheet.controller.checkJson
                                   ? JSON.parse(sheet.controller.checkJson)
                                   : ({ problems: [], warnings: [], cautions: [] })
    // `{ operands, durations }` from the controller.
    property var catalogue: ({ operands: [], durations: [] })
    property bool showAdvanced: false
    // The time is typed, not a preset; typing "1h" on the way to "1h30m"
    // mustn't switch to the "1 hour" preset and hide the field.
    property bool customDuration: false
    // Save was refused over cautions; the next click confirms them.
    property bool confirming: false
    readonly property bool renaming: !!sheet.controller
                                     && sheet.controller.editingName.length > 0
                                     && sheet.draft.name !== sheet.controller.editingName
    readonly property var actions: [
        { value: "deny", label: "Deny" },
        { value: "allow", label: "Allow" },
        { value: "reject", label: "Reject" }
    ]
    readonly property var kindLabels: ({ exact: "is", pattern: "matches the pattern",
                                         network: "is in the network" })

    // Open on a draft (JSON); returns whether it opened.
    function begin(draftJson) {
        if (!draftJson) return false;
        if (sheet.catalogue.operands.length === 0) {
            sheet.catalogue = JSON.parse(sheet.controller.catalogueJson());
        }
        sheet.draft = JSON.parse(draftJson);
        sheet.showAdvanced = sheet.draft.precedence || sheet.draft.nolog;
        sheet.customDuration = !sheet.catalogue.durations.some(function (d) {
            return d.value === sheet.draft.duration;
        });
        sheet.confirming = false;
        sheet.open();
        return true;
    }

    function startNew() {
        return !!sheet.controller && sheet.begin(sheet.controller.newRule());
    }

    // `formJson`: the simulator's prefill form for a connection.
    function startPrefill(formJson) {
        return !!sheet.controller && sheet.begin(sheet.controller.prefill(formJson));
    }

    // `editableJson`: `RulesModel.editableRuleJson`.
    function startEdit(editableJson) {
        return !!sheet.controller && sheet.begin(sheet.controller.load(editableJson));
    }

    function update(changes) {
        sheet.draft = Object.assign({}, sheet.draft, changes);
        sheet.confirming = false;
        if (sheet.controller) sheet.controller.check(JSON.stringify(sheet.draft));
    }

    function setCondition(index, changes) {
        const list = sheet.draft.conditions.slice();
        list[index] = Object.assign({}, list[index], changes);
        sheet.update({ conditions: list });
    }

    function removeCondition(index) {
        const list = sheet.draft.conditions.slice();
        list.splice(index, 1);
        sheet.update({ conditions: list });
    }

    function operandInfo(operand) {
        return sheet.catalogue.operands.find(function (o) { return o.operand === operand; })
            || { operand: operand, kinds: ["exact"], help: "" };
    }

    // A changed operand keeps the value, takes its first offered kind if the
    // old one isn't offered, and defaults case-sensitivity on for paths.
    function chooseOperand(index, operand) {
        const kinds = sheet.operandInfo(operand).kinds;
        const kind = sheet.draft.conditions[index].kind;
        sheet.setCondition(index, {
            operand: operand,
            kind: kinds.indexOf(kind) >= 0 ? kind : kinds[0],
            caseSensitive: operand === "process.path" || operand === "process.parent.path"
        });
    }

    function addCondition() {
        const list = sheet.draft.conditions.concat([{ operand: "process.path", kind: "exact",
                                                       value: "", caseSensitive: true }]);
        sheet.update({ conditions: list });
    }

    function durationIndex() {
        const i = sheet.catalogue.durations.findIndex(function (d) {
            return d.value === sheet.draft.duration;
        });
        return i >= 0 && !sheet.customDuration ? i : sheet.catalogue.durations.length;
    }

    function chooseDuration(index) {
        const presets = sheet.catalogue.durations;
        sheet.customDuration = index >= presets.length;
        sheet.update({ duration: sheet.customDuration ? "30m" : presets[index].value });
    }

    function save() {
        if (!sheet.controller) return;
        const sent = sheet.controller.submit(JSON.stringify(sheet.draft), sheet.confirming);
        if (!sent && sheet.checked.problems.length === 0 && sheet.checked.cautions.length > 0) {
            sheet.confirming = true;
        }
    }

    // Closed after a save: its result stays, shown under the Rules page's
    // title (a renamed rule's note, say). Closed any other way while not
    // waiting: nothing is left to say.
    property bool closingSaved: false

    Connections {
        target: sheet.controller
        function onSaved() {
            sheet.closingSaved = true;
            sheet.close();
        }
    }

    onClosed: {
        if (!sheet.closingSaved && sheet.controller && !sheet.controller.busy) {
            sheet.controller.statusText = "";
        }
        sheet.closingSaved = false;
    }

    Timer {
        interval: 1000
        repeat: true
        running: !!sheet.controller && sheet.controller.busy
        onTriggered: sheet.controller.poll()
    }

    ColumnLayout {
        Layout.preferredWidth: sheet.preferredWidth
        spacing: Kirigami.Units.largeSpacing

        Kirigami.FormLayout {
            Layout.fillWidth: true

            RowLayout {
                Kirigami.FormData.label: "Name"
                Controls.TextField {
                    objectName: "editorName"
                    Layout.fillWidth: true
                    text: sheet.draft.name
                    onTextEdited: sheet.update({ name: text })
                }
                Controls.Button {
                    objectName: "editorSuggestName"
                    text: "Suggest"
                    onClicked: sheet.update({
                        name: sheet.controller.suggestName(JSON.stringify(sheet.draft))
                    })
                }
            }
            Controls.ComboBox {
                objectName: "editorAction"
                Kirigami.FormData.label: "Action"
                model: sheet.actions
                textRole: "label"
                currentIndex: sheet.actions.findIndex(function (a) {
                    return a.value === sheet.draft.action;
                })
                onActivated: function (index) {
                    sheet.update({ action: sheet.actions[index].value });
                }
            }
            Controls.ComboBox {
                objectName: "editorDuration"
                Kirigami.FormData.label: "Lasts"
                model: sheet.catalogue.durations.map(function (d) { return d.label; })
                       .concat(["Custom time"])
                currentIndex: sheet.durationIndex()
                onActivated: function (index) {
                    sheet.chooseDuration(index);
                }
            }
            Controls.TextField {
                objectName: "editorCustomDuration"
                Kirigami.FormData.label: "Time"
                visible: sheet.customDuration
                placeholderText: "Such as 30s, 5m or 1h30m"
                text: sheet.draft.duration
                onTextEdited: sheet.update({ duration: text })
            }
            Controls.Switch {
                objectName: "editorEnabled"
                Kirigami.FormData.label: "On"
                checked: sheet.draft.enabled
                onToggled: sheet.update({ enabled: checked })
            }
            Controls.TextField {
                objectName: "editorDescription"
                Kirigami.FormData.label: "Description"
                text: sheet.draft.description
                onTextEdited: sheet.update({ description: text })
            }
        }

        Controls.Label {
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: "Conditions: the rule matches a connection when all of them match."
            font.bold: true
            wrapMode: Text.Wrap
        }

        // By count, not by array: retyping a value mustn't rebuild the rows.
        Repeater {
            model: sheet.draft.conditions.length

            ColumnLayout {
                id: conditionRow
                required property int index
                readonly property var condition: sheet.draft.conditions[index] || ({})
                readonly property var info: sheet.operandInfo(conditionRow.condition.operand)
                Layout.fillWidth: true
                spacing: 0

                RowLayout {
                    Layout.fillWidth: true
                    Controls.ComboBox {
                        objectName: "editorOperand"
                        model: sheet.catalogue.operands.map(function (o) {
                            return o.group + ": " + o.label;
                        })
                        currentIndex: sheet.catalogue.operands.findIndex(function (o) {
                            return o.operand === conditionRow.condition.operand;
                        })
                        onActivated: function (i) {
                            sheet.chooseOperand(conditionRow.index, sheet.catalogue.operands[i].operand);
                        }
                    }
                    Controls.ComboBox {
                        objectName: "editorKind"
                        model: conditionRow.info.kinds.map(function (k) { return sheet.kindLabels[k]; })
                        currentIndex: conditionRow.info.kinds.indexOf(conditionRow.condition.kind)
                        onActivated: function (i) {
                            sheet.setCondition(conditionRow.index, { kind: conditionRow.info.kinds[i] });
                        }
                    }
                    Controls.TextField {
                        objectName: "editorValue"
                        Layout.fillWidth: true
                        text: conditionRow.condition.value || ""
                        onTextEdited: sheet.setCondition(conditionRow.index, { value: text })
                    }
                    Controls.Button {
                        objectName: "editorRemoveCondition"
                        icon.name: "list-remove"
                        text: "Remove"
                        display: Controls.AbstractButton.IconOnly
                        onClicked: sheet.removeCondition(conditionRow.index)
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    visible: conditionRow.condition.kind !== "network"
                    Controls.CheckBox {
                        objectName: "editorCaseSensitive"
                        checked: !!conditionRow.condition.caseSensitive
                        onToggled: sheet.setCondition(conditionRow.index, { caseSensitive: checked })
                    }
                    Controls.Label {
                        textFormat: Text.PlainText
                        text: "Match upper and lower case exactly"
                    }
                }
                Controls.Label {
                    objectName: "editorOperandHelp"
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    text: conditionRow.info.help
                    color: Kirigami.Theme.disabledTextColor
                    wrapMode: Text.Wrap
                }
            }
        }

        Controls.Button {
            objectName: "editorAddCondition"
            text: "Add condition"
            icon.name: "list-add"
            onClicked: sheet.addCondition()
        }

        Controls.Button {
            objectName: "editorShowAdvanced"
            flat: true
            text: sheet.showAdvanced ? "Hide advanced options" : "Show advanced options"
            onClicked: sheet.showAdvanced = !sheet.showAdvanced
        }
        Kirigami.FormLayout {
            Layout.fillWidth: true
            visible: sheet.showAdvanced
            Controls.Switch {
                objectName: "editorPrecedence"
                Kirigami.FormData.label: "Decide first"
                checked: sheet.draft.precedence
                onToggled: sheet.update({ precedence: checked })
            }
            Controls.Switch {
                objectName: "editorNolog"
                Kirigami.FormData.label: "Don't log"
                checked: sheet.draft.nolog
                onToggled: sheet.update({ nolog: checked })
            }
        }

        Kirigami.Separator {
            Layout.fillWidth: true
        }

        Repeater {
            model: sheet.checked.problems
            Controls.Label {
                objectName: "editorProblem"
                required property string modelData
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: modelData
                color: Kirigami.Theme.negativeTextColor
                wrapMode: Text.Wrap
            }
        }
        Repeater {
            model: sheet.checked.warnings
            Controls.Label {
                objectName: "editorWarning"
                required property string modelData
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: modelData
                color: Kirigami.Theme.neutralTextColor
                wrapMode: Text.Wrap
            }
        }
        Repeater {
            model: sheet.checked.cautions
            Controls.Label {
                objectName: "editorCaution"
                required property string modelData
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: modelData
                color: Kirigami.Theme.negativeTextColor
                wrapMode: Text.Wrap
            }
        }
        Controls.Label {
            objectName: "editorRenameNote"
            visible: sheet.renaming
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: "Renaming saves the rule under its new name, then removes the old one. "
                + "Until then both exist; if removing fails, Snitchwatch says which rules are left."
            wrapMode: Text.Wrap
        }
        Controls.Label {
            objectName: "editorStatus"
            visible: text.length > 0
            Layout.fillWidth: true
            textFormat: Text.PlainText
            text: sheet.controller ? sheet.controller.statusText : ""
            wrapMode: Text.Wrap
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.largeSpacing
            Controls.Button {
                objectName: "editorCancel"
                Layout.fillWidth: true
                text: "Cancel"
                onClicked: sheet.close()
            }
            Controls.Button {
                objectName: "editorSave"
                Layout.fillWidth: true
                enabled: !!sheet.controller && !sheet.controller.busy
                         && sheet.checked.problems.length === 0
                text: sheet.confirming ? "Save anyway" : "Save"
                icon.name: "document-save"
                onClicked: sheet.save()
            }
        }
    }
}
