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

    // PageRow creates Components under a QtObject before inserting them. A
    // synchronous parentless handoff avoids that incompatible visual parent;
    // ColumnView still owns and destroys each page when it leaves the stack.
    function createPage(component, properties) {
        if (!component || component.status !== Component.Ready) {
            throw new Error("Page component is not ready");
        }
        const page = component.createObject(null, properties || {});
        if (!page) {
            throw new Error("Could not create page: " + component.errorString());
        }
        return page;
    }

    function push(component, properties) {
        return stack.push(createPage(component, properties));
    }

    function replace(component, properties) {
        return stack.replace(createPage(component, properties));
    }

    // Replace the root page with `component`; returns the new page.
    function replaceMain(component) {
        const page = createPage(component);
        if (stack.depth > 1) {
            stack.pop(stack.get(0));
        }
        return stack.replace(page);
    }
}
