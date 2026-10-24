// Observed country → city → IP map with one shared camera transform.
// Hierarchy snapshots are coalesced and cached independently of navigation.
// Country-only models retain the original cluster interface for isolated probes.
import QtQuick
import QtQuick.Shapes
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: view

    // GeoModel and ConnectionsModel; either may be null (isolated tests).
    property var geoModel: null
    property var connectionsModel: null
    property bool ready: false
    // Animations are off when the platform asks for none (animation speed 0
    // reports longDuration 0). A probe can force it.
    property bool reducedMotion: Kirigami.Units.longDuration <= 1
    // Clusters, as parsed from GeoModel.clustersJson.
    property var markers: []
    // Stable membership fingerprints preserve delegates and pulse throttles.
    property var clusterByKey: ({})
    // Country code -> its cluster's key, so a pulse needs no scan of members.
    property var keyByCode: ({})
    readonly property var emptyCluster: ({ members: [], total: 0, dominant: "none", mixed: false, summary: "", x: 0, y: 0 })
    // Pulse throttle state per country code, and refresh bookkeeping.
    property var lastPulseMs: ({})
    property int pulseIntervalMs: 500
    property int refreshCount: 0
    property int refreshCooldownMs: 250
    property bool refreshQueued: false
    // Markers closer than this on screen merge into one.
    property real markerGap: Kirigami.Units.gridUnit * 2.2
    // Markers within this many pixels merge.
    property real mergeRadius: Kirigami.Units.gridUnit * 2.5

    property alias hierarchy: hierarchy
    property alias activeGeoScope: drilldown.activeGeoScope
    property alias highlightedDestinations: drilldown.highlightedDestinations
    property alias highlightedRows: drilldown.highlightedRows
    property alias highlightedKeys: drilldown.highlightedKeys
    property int semanticLevel: 0
    property bool hierarchyDirty: true
    property var countryAnchors: []
    property var semanticCache: ({})
    readonly property var scopeNodes: hierarchy.available ? hierarchy.children(activeGeoScope.kind,activeGeoScope.key) : []
    MapHierarchy { id: hierarchy }
    MapDrilldown { id: drilldown; mapView: view }

    function loadHierarchy() {
        if (!hierarchyDirty || !geoModel || typeof geoModel.hierarchyJson !== "function") return;
        hierarchyDirty=false;
        try {
            // Country anchors are retained only as country-level fallbacks.
            countryAnchors=JSON.parse(geoModel.clustersJson(100,0,0));
            hierarchy.load(JSON.parse(geoModel.hierarchyJson()),countryAnchors);
            semanticCache={};
            highlightRows(highlightedRows);
        } catch (error) { console.warn("Geographic snapshot unavailable: "+error); }
    }
    function updateSemanticLevel() {
        if (activeGeoScope.kind==="city" || activeGeoScope.kind==="ip") { semanticLevel=2;return; }
        const zoom=camera.zoom;
        if (semanticLevel===0 && zoom>=3) semanticLevel=1;
        else if (semanticLevel===1 && zoom<2.4) semanticLevel=0;
        else if (semanticLevel===1 && zoom>=10) semanticLevel=2;
        else if (semanticLevel===2 && zoom<8) semanticLevel=zoom<2.4 ? 0 : 1;
        if (activeGeoScope.kind==="country") semanticLevel=Math.max(1,semanticLevel);
    }
    // Keep the map's public interaction surface while its controller owns
    // semantic scope and selection state independently of display delegates.
    function focusNodes(nodes, nextZoom) { drilldown.focusNodes(nodes,nextZoom); }
    function selectNode(node) { drilldown.selectNode(node); }
    function syncScopeFromModel() { drilldown.syncScopeFromModel(); }
    function clearScope() { drilldown.clearScope(); }
    function scopeBack() { drilldown.scopeBack(); }
    function highlightRows(ids) { drilldown.highlightRows(ids); }
    function isHighlighted(cluster) { return drilldown.isHighlighted(cluster); }

    // The full equirectangular world includes polar database estimates.
    readonly property real gridWidth: camera.gridWidth
    readonly property real gridTop: camera.gridTop
    readonly property real gridHeight: camera.gridHeight
    readonly property real mapScale: camera.scale
    readonly property real mapLeft: camera.left
    readonly property real mapTop: camera.top
    property alias camera: camera
    property alias landItem: land
    MapCamera {
        id: camera
        viewportWidth: view.width
        viewportHeight: view.height
    }

    readonly property bool dbMissing: !!geoModel && geoModel.dbAvailable === false
    readonly property bool empty: markers.length === 0
    readonly property string landPath: geoModel ? geoModel.landPath() : ""
    // For probes.
    property alias markerRepeater: repeater
    property alias emptyState: emptyState
    property alias countryMenu: countryMenu

    readonly property color oceanColor: Qt.tint(Kirigami.Theme.backgroundColor,
        Qt.alpha(Kirigami.Theme.highlightColor, 0.07))
    readonly property color landColor: Qt.tint(Kirigami.Theme.backgroundColor,
        Qt.alpha(Kirigami.Theme.textColor, 0.16))
    readonly property color borderColor: Qt.alpha(Kirigami.Theme.textColor, 0.22)

    Accessible.name: "World map of observed connection destinations by approximate database location"
    Accessible.role: Accessible.Graphic
    clip: true
    activeFocusOnTab: true
    Accessible.description: "Use plus and minus to zoom, arrow keys to pan, and Home to fit the world."
    Keys.onPressed: event => {
        if (event.key === Qt.Key_Plus || event.key === Qt.Key_Equal) camera.zoomAt(1.5, width / 2, height / 2);
        else if (event.key === Qt.Key_Minus) camera.zoomAt(1 / 1.5, width / 2, height / 2);
        else if (event.key === Qt.Key_Home || event.key === Qt.Key_0) camera.reset();
        else if (event.key === Qt.Key_Left) camera.panBy(60, 0);
        else if (event.key === Qt.Key_Right) camera.panBy(-60, 0);
        else if (event.key === Qt.Key_Up) camera.panBy(0, 60);
        else if (event.key === Qt.Key_Down) camera.panBy(0, -60);
        else return;
        event.accepted = true;
    }

    // Rebuild the clusters now. A model reset arrives on every connection
    // event, so callers go through requestRefresh().
    function refresh() {
        if (!view.ready) return;
        view.refreshCount++;
        view.loadHierarchy();
        view.updateSemanticLevel();
        if (hierarchy.available) {
            const bucket=Math.floor(Math.log(Math.max(view.mapScale,0.001))/Math.LN2*4);
            const cacheKey=hierarchy.revision+":"+semanticLevel+":"+activeGeoScope.kind+":"+activeGeoScope.key+":"+bucket;
            if (!semanticCache[cacheKey] && Object.keys(semanticCache).length>=8) semanticCache={};
            if (!semanticCache[cacheKey]) semanticCache[cacheKey]=hierarchy.clusters(
                hierarchy.nodes(semanticLevel,activeGeoScope.kind,activeGeoScope.key),Math.pow(2,bucket/4),Math.max(markerGap,Kirigami.Units.gridUnit*2.2+Kirigami.Units.smallSpacing*2));
            view.applyClusters(semanticCache[cacheKey]);return;
        }
        if (!view.geoModel) {
            view.applyClusters([]);
            return;
        }
        // A collapsed map has no size, but the country list and count still
        // need its markers; cluster at a nominal scale then.
        const scale = view.mapScale > 0 ? view.mapScale : 0.15;
        try {
            view.applyClusters(JSON.parse(
                view.geoModel.clustersJson(scale, view.mergeRadius, view.markerGap)));
        } catch (e) {
            view.applyClusters([]);
        }
    }

    // At most one refresh per cooldown: the first runs at once, later ones
    // within the cooldown collapse into one trailing refresh.
    function requestRefresh() {
        if (refreshCooldown.running) {
            view.refreshQueued = true;
            return;
        }
        view.refresh();
        refreshCooldown.start();
    }

    // Update `markers`, and sync `markerModel` in place: keep delegates whose
    // key survives, add or remove only the difference.
    function applyClusters(clusters) {
        // Bound labels separately from geography. Nearby button centers never
        // acquire invented positions; long names elide and remain in the tip.
        for (const cluster of clusters) {
            let labelWidth=160;
            for (const other of clusters) {
                if (other===cluster) continue;
                const dx=Math.abs(cluster.x-other.x)*view.mapScale;
                const dy=Math.abs(cluster.y-other.y)*view.mapScale;
                if (dy<28 && other.members.length===1) labelWidth=Math.min(labelWidth,dx-8);
            }
            cluster.labelWidth=Math.max(0,labelWidth);
        }
        const byKey = {};
        const byCode = {};
        const keys = [];
        for (const c of clusters) {
            const key = c.key || c.members[0].code;
            byKey[key] = c;
            keys.push(key);
            for (const m of c.members) {
                byCode[m.code] = key;
            }
        }
        view.clusterByKey = byKey;
        view.keyByCode = byCode;
        view.markers = clusters;
        for (let i = markerModel.count - 1; i >= 0; i--) {
            if (!byKey.hasOwnProperty(markerModel.get(i).key)) {
                markerModel.remove(i);
            }
        }
        for (let i = 0; i < keys.length; i++) {
            if (i < markerModel.count && markerModel.get(i).key === keys[i]) {
                continue;
            }
            let from = -1;
            for (let j = i + 1; j < markerModel.count; j++) {
                if (markerModel.get(j).key === keys[i]) { from = j; break; }
            }
            if (from >= 0) {
                markerModel.move(from, i, 1);
            } else {
                markerModel.insert(i, { key: keys[i] });
            }
        }
    }
    onMapScaleChanged: requestRefresh()
    onGeoModelChanged: { hierarchy.clear();semanticCache={};hierarchyDirty=true;requestRefresh(); }
    Component.onCompleted: {
        view.ready=true;
        view.requestRefresh();
    }

    Timer {
        id: refreshCooldown
        interval: view.refreshCooldownMs
        onTriggered: {
            if (view.refreshQueued) {
                view.refreshQueued = false;
                view.refresh();
                refreshCooldown.start();
            }
        }
    }
    ListModel { id: markerModel }

    Connections {
        target: view.geoModel
        ignoreUnknownSignals: true
        function onModelReset() { view.requestRefresh(); }
        function onHierarchyChanged() { view.hierarchyDirty=true; view.requestRefresh(); }
        function onCountryActivity(code) { view.pulse(code, Date.now()); }
    }

    // Filter the Connections list to one country.
    function filterCountry(code) {
        if (view.connectionsModel) {
            view.connectionsModel.setFilterQuery("country:" + code);
        }
    }

    // A marker was activated: a lone country filters; a cluster offers its
    // countries.
    function activate(cluster, anchor) {
        if (cluster.members.length === 1) {
            if (hierarchy.available) view.selectNode(cluster.members[0]);
            else view.filterCountry(cluster.members[0].code);
        } else {
            view.openCountryMenu(cluster.members, anchor);
            if (!hierarchy.available || cluster.members.some(m=>m.kind!=="ip")) camera.focusPoint(cluster.x, cluster.y, camera.zoom * 2);
        }
    }

    // List `members` ({code, name, summary}) in a menu: a keyboard-reachable
    // alternative to hitting the markers.
    function openCountryMenu(members, anchor) {
        countryMenu.members = members;
        countryMenu.popup(anchor);
    }

    // All countries currently shown, busiest first.
    function allMembers() {
        if (hierarchy.available) return hierarchy.countries;
        const out = [];
        for (const c of view.markers) {
            for (const m of c.members) {
                out.push(m);
            }
        }
        out.sort((a, b) => b.total - a.total || (a.code < b.code ? -1 : 1));
        return out;
    }

    // Pulse the marker showing `code`: at most once per interval per country
    // (state here, so it survives marker changes), and never under reduced
    // motion.
    function pulse(code, nowMs) {
        // Nothing to animate while the band is collapsed or the map hidden.
        if (view.reducedMotion || !view.visible) {
            return false;
        }
        const last = view.lastPulseMs[code];
        if (last !== undefined && nowMs - last < view.pulseIntervalMs) {
            return false;
        }
        const key = view.keyByCode[code];
        for (let i = 0; key !== undefined && i < markerModel.count; i++) {
            if (markerModel.get(i).key === key) {
                const item = repeater.itemAt(i);
                if (!item) {
                    return false;
                }
                view.lastPulseMs[code] = nowMs;
                return item.pulse(nowMs);
            }
        }
        return false;
    }

    Rectangle {
        anchors.fill: parent
        color: view.oceanColor
    }

    MapNavigation { anchors.fill: parent; camera: view.camera }

    Shape {
        id: land
        visible: view.landPath !== ""
        // Grid (0, 0) sits at the item origin; scaling about it and
        // offsetting the item places the cropped grid in the band.
        x: view.mapLeft
        y: view.mapTop - view.gridTop * view.mapScale
        transformOrigin: Item.TopLeft
        scale: view.mapScale
        // Not asynchronous: a screenshot must not catch it half built.
        ShapePath {
            fillColor: view.landColor
            strokeColor: view.borderColor
            // Keep the fitted-world appearance while zoom adds detail.
            strokeWidth: 5 / camera.zoom
            joinStyle: ShapePath.RoundJoin
            PathSvg { path: view.landPath }
        }
    }

    Repeater {
        id: repeater
        model: markerModel
        delegate: MapMarker {
            id: mk
            required property string key
            cluster: view.clusterByKey[key] || emptyCluster
            reducedMotion: view.reducedMotion
            highlighted: view.isHighlighted(cluster)
            labelWidth: Math.max(0,Math.min(cluster.labelWidth===undefined ? 160 : cluster.labelWidth,
                2*(mk.x+mk.width/2-4),2*(view.width-mk.x-mk.width/2-4)))
            x: view.mapLeft + cluster.x * view.mapScale - width / 2
            y: view.mapTop + (cluster.y - view.gridTop) * view.mapScale - height / 2
            enabled: x + width > 0 && y + height > 0 && x < view.width && y < view.height
            activeFocusOnTab: enabled
            Accessible.ignored: !enabled
            onClicked: view.activate(cluster, mk)
        }
    }

    // Never nothing: say why the map is bare.
    Column {
        id: emptyState
        visible: view.empty
        anchors.centerIn: parent
        width: Math.min(parent.width - Kirigami.Units.largeSpacing * 2, Kirigami.Units.gridUnit * 30)
        spacing: Kirigami.Units.smallSpacing
        Controls.Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            font.bold: true
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: view.dbMissing ? "No GeoIP database installed"
                : hierarchy.available && activeGeoScope.kind!=="" && scopeNodes.length===0 ? "No observed destinations in this scope"
                : hierarchy.available && hierarchy.snapshot.observedConnections>0 ? "No coordinates for these destinations"
                : (view.geoModel ? "No destinations to place yet" : "No map data")
        }
        Controls.Label {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            text: view.dbMissing
                ? "Place a GeoLite2-Country.mmdb file at the path shown on the Geography page. Snitchwatch never downloads one automatically."
                : hierarchy.available && view.empty
                  ? "Use Destinations for observed IPs without coordinates, or World / Back to change geographic scope."
                  : "Markers appear here as connections to public addresses are seen."
        }
    }

    MapDestinationsPopup {
        id: countryMenu
        onSelected: node => {
            if (hierarchy.available) view.selectNode(node);
            else view.filterCountry(node.code);
        }
    }
}
