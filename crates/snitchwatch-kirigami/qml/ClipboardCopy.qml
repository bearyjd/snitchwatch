// Preserve the captured string without TextEdit's newline normalization.
import QtQuick
import com.snitchwatch.shell

Item {
    width: 0
    height: 0
    function copy(value) {
        writer.write(value);
    }
    NativeClipboard { id: writer }
}
