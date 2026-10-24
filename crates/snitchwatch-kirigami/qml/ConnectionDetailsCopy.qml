import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls

ColumnLayout {
    id: details
    property var snapshot: ({})
    property bool available: false
    property var validSnapshot: null
    ClipboardCopy { id: clipboard }
    Repeater {
        model: [
            { name: "copyExecutable", label: "Copy executable path", key: "processPath" },
            { name: "copyHost", label: "Copy host", key: "dstHost" },
            { name: "copyIp", label: "Copy destination IP", key: "dstIp" },
            { name: "copyPort", label: "Copy port", key: "dstPort" }
        ]
        Controls.Button {
            required property var modelData
            readonly property var value: details.snapshot[modelData.key]
            objectName: modelData.name
            text: modelData.label
            Layout.fillWidth: true
            enabled: details.available && value !== undefined && value !== null && String(value) !== "" && (modelData.key !== "dstPort" || String(value) !== "0")
            onClicked: if (details.available && (!details.validSnapshot || details.validSnapshot())) clipboard.copy(String(value))
        }
    }
}
