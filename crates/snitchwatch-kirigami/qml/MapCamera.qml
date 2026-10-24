// Equirectangular grid camera. Every coordinate uses this same transform.
import QtQuick

QtObject {
    id: camera
    property real viewportWidth: 0
    property real viewportHeight: 0
    readonly property real gridWidth: 3600
    readonly property real gridTop: 0
    readonly property real gridHeight: 1800
    property real minimumZoom: 1
    property real maximumZoom: 128
    readonly property real lowerZoom: isFinite(minimumZoom) ? Math.max(1, minimumZoom) : 1
    readonly property real upperZoom: isFinite(maximumZoom) ? Math.max(lowerZoom, maximumZoom) : lowerZoom
    readonly property real fitScale: viewportWidth > 0 && viewportHeight > 0
        ? Math.min(viewportWidth / gridWidth, viewportHeight / gridHeight) : 0
    readonly property real scale: fitScale * zoom
    readonly property real left: viewportWidth / 2 - centerX * scale
    readonly property real top: viewportHeight / 2 - (centerY - gridTop) * scale
    property real zoom: 1
    property real centerX: 1800
    property real centerY: 900
    signal moved()

    function project(x, y) {
        return Qt.point(left + x * scale, top + (y - gridTop) * scale);
    }
    function unproject(x, y) {
        return scale > 0 ? Qt.point((x - left) / scale, gridTop + (y - top) / scale)
                         : Qt.point(centerX, centerY);
    }
    function boundedCenter(value, start, length, viewport, nextScale) {
        const half = Math.min(length / 2, viewport / (2 * nextScale));
        return Math.max(start + half, Math.min(start + length - half, value));
    }
    function setCamera(nextZoom, x, y) {
        if (!isFinite(nextZoom) || !isFinite(x) || !isFinite(y)) return false;
        zoom = Math.max(lowerZoom, Math.min(upperZoom, nextZoom));
        centerX = scale > 0 ? boundedCenter(x, 0, gridWidth, viewportWidth, scale) : x;
        centerY = scale > 0 ? boundedCenter(y, gridTop, gridHeight, viewportHeight, scale) : y;
        moved();
        return true;
    }
    function zoomAt(factor, x, y) {
        if (!(factor > 0) || !isFinite(factor) || scale <= 0) return;
        const anchor = unproject(x, y);
        const nextZoom = Math.max(lowerZoom, Math.min(upperZoom, zoom * factor));
        const nextScale = fitScale * nextZoom;
        setCamera(nextZoom, anchor.x - (x - viewportWidth / 2) / nextScale,
                  anchor.y - (y - viewportHeight / 2) / nextScale);
    }
    function panBy(dx, dy) {
        if (scale > 0) setCamera(zoom, centerX - dx / scale, centerY - dy / scale);
    }
    function reset() { setCamera(1, gridWidth / 2, gridTop + gridHeight / 2); }
    function focusPoint(x, y, nextZoom) { return setCamera(nextZoom, x, y); }
    // Bounds are ordinary grid coordinates, not wrapped longitudes. A caller
    // crossing the date line must choose a visible extent before fitting it.
    function fitBounds(minX, minY, maxX, maxY, padding) {
        if (![minX, minY, maxX, maxY, padding].every(isFinite)
                || maxX < minX || maxY < minY || padding < 0 || fitScale <= 0) return false;
        const fit = Math.min(Math.max(1, viewportWidth - 2 * padding) / Math.max(1, maxX - minX),
                             Math.max(1, viewportHeight - 2 * padding) / Math.max(1, maxY - minY));
        return setCamera(fit / fitScale, (minX + maxX) / 2, (minY + maxY) / 2);
    }
    onViewportWidthChanged: setCamera(zoom, centerX, centerY)
    onViewportHeightChanged: setCamera(zoom, centerX, centerY)
    onLowerZoomChanged: setCamera(zoom, centerX, centerY)
    onUpperZoomChanged: setCamera(zoom, centerX, centerY)
}
