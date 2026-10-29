// A ListView whose Tab order is its visual order (Q01).
//
// Qt walks the focus chain in the order of an item's children, and a
// ListView creates its header, footer and rows when they are needed, not top
// to bottom: a footer made before the rows is tabbed to before them. After
// the rows or the content change, the footer is made the last child again,
// which puts it last in the chain. (The header is made first, and rows are
// added in order.) A footer holding focus is left alone until it does not.
import QtQuick

ListView {
    id: view

    function restack() {
        const footer = view.footerItem;
        if (!footer || !view.contentItem) return;
        const kids = view.contentItem.children;
        if (kids.length === 0 || kids[kids.length - 1] === footer) return;
        for (let it = view.Window.activeFocusItem; it; it = it.parent) {
            if (it === footer) return;
        }
        const holder = footer.parent;
        footer.parent = null;
        footer.parent = holder;
    }

    onCountChanged: Qt.callLater(view.restack)
    onContentHeightChanged: Qt.callLater(view.restack)
    // A Kirigami page makes its flickable a Tab stop of its own, after the
    // page has been built; the rows are the stops here.
    Component.onCompleted: Qt.callLater(function () {
        view.activeFocusOnTab = false;
        view.restack();
    })
}
