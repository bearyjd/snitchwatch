// A row of mutually exclusive buttons ("segments"): the alert's scope and
// duration choosers. `model` is a list of `{ label, token }`; the chosen
// token is `currentToken`, which falls back to the first entry when the
// model no longer holds it (`effectiveToken`) — a shrunken duration list
// then reads "Once" without forgetting what the user picked.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls

Item {
    id: seg

    property var model: []
    property string currentToken: ""
    // Names the group for a screen reader: "<groupName>: <label>".
    property string groupName: ""
    // Every key pressed or released on a choice, for the owner's held-key
    // bookkeeping (a choice consumes Return and a native Space).
    signal keyDown(int key, bool autoRepeat)
    signal keyUp(int key, bool autoRepeat)
    readonly property string effectiveToken: seg.indexOfToken(seg.currentToken) >= 0
        ? seg.currentToken : (seg.model.length > 0 ? seg.model[0].token : "")

    implicitWidth: strip.implicitWidth
    implicitHeight: strip.implicitHeight

    function indexOfToken(token) {
        for (let i = 0; i < seg.model.length; i++) {
            if (seg.model[i].token === token) {
                return i;
            }
        }
        return -1;
    }

    // The index of the choice button `item`, or -1.
    function indexOfItem(item) {
        for (let i = 0; i < rep.count; i++) {
            if (rep.itemAt(i) === item) {
                return i;
            }
        }
        return -1;
    }

    // Focus the choice button at `index`, or the last one if there are fewer.
    function focusAt(index) {
        const item = rep.itemAt(Math.min(index, rep.count - 1));
        if (item !== null) {
            item.forceActiveFocus();
        }
    }

    RowLayout {
        id: strip
        anchors.fill: parent
        spacing: 0

        Repeater {
            id: rep
            model: seg.model
            delegate: Controls.Button {
                required property var modelData
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                text: modelData.label
                highlighted: modelData.token === seg.effectiveToken
                Accessible.name: seg.groupName !== "" ? seg.groupName + ": " + modelData.label : modelData.label
                Accessible.checked: highlighted
                onClicked: seg.currentToken = modelData.token
                // Q01: Return on a focused choice only selects it, as a click
                // does; it never goes on to the window's Return = Allow.
                Keys.onReleased: event => seg.keyUp(event.key, event.isAutoRepeat)
                Keys.onPressed: event => {
                    // Only keys the choice consumes; the rest reach the owner's
                    // own handler, which records them.
                    if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space) {
                        seg.keyDown(event.key, event.isAutoRepeat);
                    }
                    if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                        event.accepted = true;
                        if (!event.isAutoRepeat) {
                            seg.currentToken = modelData.token;
                        }
                    }
                }
            }
        }
    }
}
