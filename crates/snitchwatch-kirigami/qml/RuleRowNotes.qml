// A Rules page row's note that the connected daemon skips the rule
// (`rules::daemon_refusal`); hidden when `reason` is empty. Split out of
// RulesPage.qml to keep it under the file-size cap.
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Controls.Label {
    objectName: "notLoadedReason"
    // The row's `notLoadedReason` role; empty when the daemon loads the rule.
    property string reason: ""
    visible: text.length > 0
    textFormat: Text.PlainText
    text: reason
    wrapMode: Text.Wrap
    font: Kirigami.Theme.smallFont
    color: Kirigami.Theme.negativeTextColor
}
