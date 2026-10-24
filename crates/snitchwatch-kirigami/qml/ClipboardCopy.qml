// A GUI-only clipboard writer. Plain text preserves captured bytes literally.
import QtQuick

Item {
    id: clipboard
    width: 0
    height: 0
    function copy(value) {
        buffer.text = value;
        buffer.selectAll();
        buffer.copy();
        buffer.text = "";
    }
    TextEdit {
        id: buffer
        visible: false
        textFormat: TextEdit.PlainText
    }
}
