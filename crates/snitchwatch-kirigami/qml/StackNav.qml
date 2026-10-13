// Top-level navigation on the window's page stack. A pushed page (the
// Connections detail page in a narrow window) sits above the page the user
// navigates away from, and PageRow.replace swaps the TOPMOST page: without
// first popping back to the root, the detail page would be replaced and the
// Connections page would stay stacked underneath. main.qml routes every
// "go to page X" through here.
import QtQuick

QtObject {
    // The window's PageRow.
    property var stack

    // Replace the root page with `component`; returns the new page.
    function replaceMain(component) {
        if (stack.depth > 1) {
            stack.pop(stack.get(0));
        }
        return stack.replace(component);
    }
}
