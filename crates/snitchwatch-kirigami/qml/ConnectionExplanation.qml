// Offline explanation of the captured record; no lookup or policy replay.
import QtQuick

SelectableText {
    id: explanation
    property var snapshot: ({})
    property string recordedVerdict: ""
    objectName: "connectionExplanation"
    textFormat: Text.PlainText
    wrapMode: Text.WrapAnywhere
    function known(value) { return value === undefined || value === null || value === "" || value === 0 ? "unknown" : String(value); }
    text: "What is this?\nThe recorded executable " + known(snapshot.processPath)
        + " has recorded destination host/IP " + known(snapshot.dstHost) + " (IP " + known(snapshot.dstIp)
        + "), port " + known(snapshot.dstPort) + ", protocol " + known(snapshot.protocol) + ".\n"
        + "Recorded verdict: " + known(recordedVerdict) + ". Recorded matched rule: " + known(snapshot.matchedRule)
        + ". These fields come from the connection record. They do not identify an owner, reputation or purpose. "
        + "These details describe the recorded connection. Current rules may differ."
}
