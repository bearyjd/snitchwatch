// Semantic navigation and selection; rendering remains in MapView.
import QtQuick

QtObject {
    id: control
    property var mapView: null
    readonly property var hierarchy: mapView ? mapView.hierarchy : null
    readonly property var camera: mapView ? mapView.camera : null
    readonly property var connectionsModel: mapView ? mapView.connectionsModel : null
    property var activeGeoScope: ({
            kind: "",
            key: "",
            name: "World",
            countryKey: "",
            cityKey: ""
        })
    property var highlightedDestinations: []
    property var highlightedRows: []
    property var highlightedKeys: ({})

    function focusNodes(nodes, nextZoom) {
        const points = nodes.map(n => n.position).filter(p => p && isFinite(p.x) && isFinite(p.y));
        if (!points.length)
            return;
        let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
        for (const point of points) {
            minX = Math.min(minX, point.x);
            maxX = Math.max(maxX, point.x);
            minY = Math.min(minY, point.y);
            maxY = Math.max(maxY, point.y);
        }
        if (maxX - minX < 1 && maxY - minY < 1)
            camera.focusPoint(minX, minY, nextZoom);
        else {
            // This map has one non-wrapping world. Date-line branches therefore
            // fit both edges rather than inventing a midpoint near Greenwich.
            camera.fitBounds(minX, minY, maxX, maxY, 24);
            if (maxX - minX < 1800 && camera.zoom < nextZoom && maxX - minX < 400 && maxY - minY < 250)
                camera.focusPoint((minX + maxX) / 2, (minY + maxY) / 2, nextZoom);
        }
    }
    function selectNode(node) {
        const key = node.key || (node.code ? "country:" + node.code : "");
        const kind = node.kind || "country";
        if (connectionsModel && typeof connectionsModel.setGeoScope === "function") {
            if (!connectionsModel.setGeoScope(kind, key))
                return;
        } else if (connectionsModel) {
            connectionsModel.setFilterQuery("country:" + node.code);
        }
        activeGeoScope = {
            kind: kind,
            key: key,
            name: node.name,
            countryKey: node.countryKey || key,
            cityKey: node.cityKey || (kind === "city" ? key : "")
        };
        mapView.semanticCache = {};
        if (kind === "country") {
            mapView.semanticLevel = 1;
            const children = hierarchy.children(kind, key).filter(n => n.position && n.approximate);
            if (children.length)
                focusNodes(children, Math.max(4, camera.zoom));
            else if (node.position)
                camera.focusPoint(node.position.x, node.position.y, Math.max(4, camera.zoom));
            // Automatic fit exposes cities first. A later deliberate zoom may
            // cross the IP threshold; live refreshes never move the camera.
            if (camera.zoom > 8)
                camera.setCamera(8, camera.centerX, camera.centerY);
            mapView.semanticLevel = 1;
        } else if (kind === "city") {
            mapView.semanticLevel = 2;
            if (node.position)
                camera.focusPoint(node.position.x, node.position.y, Math.max(12, camera.zoom));
        }
        mapView.requestRefresh();
    }
    function syncScopeFromModel() {
        if (!connectionsModel || connectionsModel.geoScopeKind === undefined)
            return;
        const kind = connectionsModel.geoScopeKind, key = connectionsModel.geoScopeKey;
        if (activeGeoScope.kind === kind && activeGeoScope.key === key)
            return;
        const node = hierarchy.byKey[key];
        activeGeoScope = {
            kind: kind,
            key: key,
            name: node ? node.name : kind ? activeGeoScope.name : "World",
            countryKey: node ? node.countryKey : activeGeoScope.countryKey,
            cityKey: node ? node.cityKey || (kind === "city" ? key : "") : activeGeoScope.cityKey
        };
        mapView.semanticCache = {};
        mapView.requestRefresh();
    }
    property QtObject scopeChanges: Connections {
        target: control.connectionsModel
        ignoreUnknownSignals: true
        function onGeoScopeKindChanged() {
            Qt.callLater(control.syncScopeFromModel);
        }
        function onGeoScopeKeyChanged() {
            Qt.callLater(control.syncScopeFromModel);
        }
    }
    function clearScope() {
        if (connectionsModel && typeof connectionsModel.setGeoScope === "function")
            connectionsModel.setGeoScope("", "");
        activeGeoScope = {
            kind: "",
            key: "",
            name: "World",
            countryKey: "",
            cityKey: ""
        };
        mapView.semanticCache = {};
        mapView.requestRefresh();
    }
    function scopeBack() {
        const s = activeGeoScope;
        const key = s.kind === "ip" ? s.cityKey : s.kind === "city" ? s.countryKey : "";
        if (key && hierarchy.byKey[key])
            selectNode(hierarchy.byKey[key]);
        else
            clearScope();
    }
    function highlightRows(ids) {
        highlightedRows = ids;
        const destinations = [], keys = {};
        for (const id of ids) {
            const key = hierarchy.destinationByRow[id];
            if (key === undefined)
                continue;
            destinations.push(key);
            keys[key] = true;
            const node = hierarchy.byKey[key];
            if (node) {
                keys[node.countryKey] = true;
                keys[node.cityKey] = true;
            }
        }
        highlightedDestinations = destinations;
        highlightedKeys = keys;
    }
    function isHighlighted(cluster) {
        return cluster.members.some(node => highlightedKeys[node.key] === true);
    }
}
