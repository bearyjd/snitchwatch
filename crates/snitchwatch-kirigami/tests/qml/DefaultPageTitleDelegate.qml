// A CUSTOM title delegate that borrows the default delegate's type name, to
// prove the tracer does not authorize a delegate subtree by name alone.
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

RowLayout {
    property string shown: ""
    property string sibling: ""
    Kirigami.Heading { objectName: "customHeading"; text: parent.shown; level: 1 }
    Text { objectName: "customSibling"; text: parent.sibling }
}
