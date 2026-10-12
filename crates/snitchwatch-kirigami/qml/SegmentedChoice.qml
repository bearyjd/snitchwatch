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

    RowLayout {
        id: strip
        anchors.fill: parent
        spacing: 0

        Repeater {
            model: seg.model
            delegate: Controls.Button {
                required property var modelData
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                text: modelData.label
                highlighted: modelData.token === seg.effectiveToken
                Accessible.checked: highlighted
                onClicked: seg.currentToken = modelData.token
            }
        }
    }
}
