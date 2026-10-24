import QtQuick
import QtQuick.Window
import QtQuick.Controls as Controls
import com.snitchwatch.shell

Window {
    id: root
    visible: true
    width: 1000; height: 900
    property int beats: 0
    property var productionWindow: null
    readonly property var sheet: productionWindow ? productionWindow.analysisExport : null
    readonly property var controller: sheet ? sheet.controller : null
    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/qt/qml/com/snitchwatch/shell/qml/main.qml");
        check(component.status === Component.Ready, "production main window did not compile");
        productionWindow = component.createObject(root, { objectName: "analysisExportMain", visible: true });
        check(productionWindow !== null && controller !== null, "main window does not own the export flow");
    }
    Timer { interval: 5; running: true; repeat: true; onTriggered: ++root.beats }
    function find(name, item) {
        if (!item) item = sheet.contentItem;
        if (item.objectName === name) return item;
        for (let i = 0; i < item.children.length; ++i) {
            const found = find(name, item.children[i]);
            if (found) return found;
        }
        return null;
    }
    function check(condition, message) { if (!condition) throw new Error(message); }
    function openPreview() {
        sheet.open();
        const preview = find("analysisExportPreview");
        check(preview.readOnly && preview.textFormat === TextEdit.PlainText, "preview can render markup");
        check(preview.text === controller.previewText, "preview text was rebuilt");
        check(find("analysisExportScope").textFormat === Text.PlainText, "scope is not plain text");
        check(find("analysisExportSave").enabled, "valid preview cannot be saved");
        return "ok";
    }
    function checkSavingLocks() {
        check(controller.saving && sheet.visible && productionWindow.visible, "save lost its dialog/window");
        check(sheet.closePolicy === Controls.Popup.NoAutoClose, "save permits Escape/outside dismissal");
        const disabled = ["Request", "Save", "Cancel", "Close", "WindowEnabled", "AppChoice", "ExactPath", "VerdictEnabled"];
        for (let i = 0; i < disabled.length; ++i)
            check(!find("analysisExport" + disabled[i]).enabled, "saving control stayed enabled");
        find("analysisExportRequest").clicked();
        find("analysisExportSave").clicked();
        find("analysisExportCancel").clicked();
        find("analysisExportClose").clicked();
        check(sheet.visible && controller.saving, "control dismissed/cancelled an admitted save");
        return "ok";
    }
    function navigateAway() {
        check(productionWindow.pageStack.depth > 0, "production page stack is empty");
        productionWindow.pageStack.clear();
        check(controller.saving && sheet.visible, "page navigation destroyed the admitted save");
        return "ok";
    }
    function checkSessionLost() {
        check(!controller.previewReady && controller.previewText === "", "lost session retained preview");
        check(controller.saving && sheet.visible, "lost session cancelled local save");
        return "ok";
    }
    function checkOutcome() {
        check(!controller.saving && !controller.previewReady, "save did not unlock or revived preview");
        check(controller.savedLocation.length > 0, "save outcome lost its local location");
        check(find("analysisExportSavedLocation").textFormat === Text.PlainText, "location is not plain text");
        check(find("analysisExportClose").enabled, "save did not unlock Close");
        check(!find("analysisExportSave").enabled, "saved preview can publish again");
        return "ok";
    }
    function cleanup() {
        productionWindow.destroy();
        productionWindow = null;
        return "ok";
    }
}
