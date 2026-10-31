// Test stub for the Rust-registered NativeClipboard (cxx-qt links it
// statically, so qmltestrunner cannot see the real one). Records writes.
import QtQuick

QtObject {
    property string lastWritten: ""
    function write(value) { lastWritten = value; }
}
