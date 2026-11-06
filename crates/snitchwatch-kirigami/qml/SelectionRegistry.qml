// Which SelectableText holds the current selection. A selectable text never
// takes keyboard focus, so Ctrl+C is routed to the one that is current:
// the last one selected wins and the others are cleared.
pragma Singleton
import QtQuick

QtObject {
    property var current: null
}
