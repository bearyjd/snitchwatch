// Test-only stand-in for the Rust ScannerController (qmltestrunner has no
// Rust types): idle, no report, never starts a scan.
import QtQuick

QtObject {
    property bool busy: false
    property string errorText: ""
    property string errorTitle: ""
    property string reportJson: ""
    property string summaryKind: ""
    property string summaryText: ""
    function takePendingOutcome() { return ""; }
    function runScan() {}
}
