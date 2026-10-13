// The world map itself: country shapes plus clustered verdict markers.
//
// Shapes are one SVG path string from Rust (GeoModel.landPath, Natural Earth
// 110m on a 0.1 degree equirectangular grid), drawn with a single ShapePath;
// nothing is loaded from disk or the network here. Markers come from
// GeoModel.clustersJson, which merges nearby countries at the current scale.
// Clicking a country filters the Connections list (`country:XX`); clicking a
// cluster lists its countries in a menu, because filter tokens are AND-ed and
// one filter cannot cover several countries.
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
    property bool reducedMotion: Kirigami.Units.longDuration === 0
    // Clusters, as parsed from GeoModel.clustersJson.
    property var markers: []
    // Markers within this many pixels merge.
    property real mergeRadius: Kirigami.Units.gridUnit * 2.5

    // Visible slice of the 3600x1800 grid: Antarctica and the high Arctic are
    // cropped so the inhabited world gets the band.
    readonly property real gridWidth: 3600
    readonly property real gridTop: 60
    readonly property real gridHeight: 1440
    readonly property real mapScale: width > 0 && height > 0
        ? Math.min(width / gridWidth, height / gridHeight) : 0
    readonly property real mapLeft: (width - gridWidth * mapScale) / 2
    readonly property real mapTop: (height - gridHeight * mapScale) / 2

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

    function refresh() {
        if (!view.geoModel) {
            view.markers = [];
            return;
        }
        // A collapsed map has no size, but the country list and count still
        // need its markers; cluster at a nominal scale then.
        const scale = view.mapScale > 0 ? view.mapScale : 0.15;
        try {
            view.markers = JSON.parse(view.geoModel.clustersJson(scale, view.mergeRadius));
        } catch (e) {
            view.markers = [];
        }
    }
    onMapScaleChanged: refresh()
    onGeoModelChanged: refresh()
    Component.onCompleted: refresh()

    Connections {
        target: view.geoModel
        ignoreUnknownSignals: true
        function onModelReset() { view.refresh(); }
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

    // Pulse the marker showing `code`; each marker throttles itself.
    function pulse(code, nowMs) {
        for (let i = 0; i < repeater.count; i++) {
            const item = repeater.itemAt(i);
            if (item && item.cluster.members.some(m => m.code === code)) {
                return item.pulse(nowMs);
            }
        }
        return false;
    }

    Rectangle {
        anchors.fill: parent
        color: view.oceanColor
    }

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
            strokeWidth: view.mapScale > 0 ? 1 / view.mapScale : 1
            joinStyle: ShapePath.RoundJoin
            PathSvg { path: view.landPath }
        }
    }

    Repeater {
        id: repeater
        model: view.markers
        delegate: MapMarker {
            id: mk
            required property var modelData
            cluster: modelData
            reducedMotion: view.reducedMotion
            x: view.mapLeft + modelData.x * view.mapScale - width / 2
            y: view.mapTop + (Math.max(view.gridTop, Math.min(view.gridTop + view.gridHeight, modelData.y))
                              - view.gridTop) * view.mapScale - height / 2
            onClicked: view.activate(modelData, mk)
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
