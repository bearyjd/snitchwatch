// The world map itself: country shapes plus clustered verdict markers.
//
// Shapes are one SVG path string from Rust (GeoModel.landPath, Natural Earth
// 110m on a 0.1 degree equirectangular grid), drawn with a single ShapePath;
// nothing is loaded from disk or the network here. Markers come from
// GeoModel.clustersJson, which merges nearby countries at the current scale.
// Clicking a country filters the Connections list (`country:XX`); clicking a
// cluster focuses its existing aggregate location and lists its countries.
//
// `markers` is a plain JS array property so a probe can set it directly.
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
    // Animations are off when the platform asks for none (animation speed 0
    // reports longDuration 0). A probe can force it.
    property bool reducedMotion: Kirigami.Units.longDuration <= 1
    // Clusters, as parsed from GeoModel.clustersJson.
    property var markers: []
    // Marker delegates live in `markerModel`, keyed by the cluster's first
    // country, so a refresh updates them in place instead of recreating them
    // (a recreated marker would forget its pulse throttle).
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

    // Visible slice of the 3600x1800 grid: Antarctica and the high Arctic are
    // cropped so the inhabited world gets the band.
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

    Accessible.name: "World map of connection destinations by approximate country"
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
        view.refreshCount++;
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
        const byKey = {};
        const byCode = {};
        const keys = [];
        for (const c of clusters) {
            const key = c.members[0].code;
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
    onGeoModelChanged: requestRefresh()
    Component.onCompleted: requestRefresh()

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
            view.filterCountry(cluster.members[0].code);
        } else {
            view.openCountryMenu(cluster.members, anchor);
            camera.focusPoint(cluster.x, cluster.y, camera.zoom * 2);
        }
    }

    // List `members` ({code, name, summary}) in a menu: a keyboard-reachable
    // alternative to hitting the markers.
    function openCountryMenu(members, anchor) {
        countryMenu.members = members;
        if (anchor) {
            countryMenu.popup(anchor, 0, anchor.height);
        } else {
            countryMenu.popup();
        }
    }

    // All countries currently shown, busiest first.
    function allMembers() {
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
                : "Markers appear here as connections to public addresses are seen."
        }
    }

    Controls.Menu {
        id: countryMenu
        property var members: []
        Repeater {
            model: countryMenu.members
            delegate: Controls.MenuItem {
                required property var modelData
                // Names come from the GeoIP file: plain text only.
                text: modelData.name + " (" + modelData.summary + ")"
                onTriggered: view.filterCountry(modelData.code)
            }
        }
    }
}
