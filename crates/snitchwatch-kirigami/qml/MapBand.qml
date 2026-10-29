// The featured map band above the connections tree (LS redesign phase 2,
// step 9): a header with a collapse chevron, the map, a legend and the honest
// footer. It is the top item of MonitorPane's vertical SplitView, whose handle
// resizes it; this file only reports the heights it needs.
//
// Visible by default. Collapse and height live in MapBandState, a session-only
// singleton (never in settings).
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: band

    property var geoModel: null
    property var connectionsModel: null
    property bool cameraReady: false
    Component.onCompleted: {
        map.activeGeoScope=MapBandState.geoScope;
        if (band.connectionsModel && typeof band.connectionsModel.setGeoScope === "function") {
            const scope=MapBandState.geoScope;
            band.connectionsModel.setGeoScope(scope.kind,scope.key);
        }
        const saved = MapBandState.camera;
        map.camera.setCamera(saved.zoom, saved.x, saved.y);
        band.cameraReady = true;
    }
    Connections {
        target: map.camera
        function onMoved() {
            if (band.cameraReady) {
                MapBandState.camera = { zoom: map.camera.zoom, x: map.camera.centerX, y: map.camera.centerY };
            }
        }
    }
    readonly property real minimumBody: Kirigami.Units.gridUnit * 6
    readonly property real defaultBody: Kirigami.Units.gridUnit * 13
    // The map is as tall as it is wide / 2.5, so more height is wasted.
    readonly property real maximumBody: Math.max(band.minimumBody,
        band.width > 0 ? band.width / 2.5 : band.defaultBody)
    // The header row and the separator: all that shows when collapsed.
    readonly property real collapsedHeight: header.implicitHeight + separator.implicitHeight
    // Everything but the map itself, when expanded (the footer's margins too).
    readonly property real chromeHeight: band.collapsedHeight
        + breadcrumbs.implicitHeight + footer.implicitHeight + footer.Layout.topMargin + footer.Layout.bottomMargin
    readonly property real bodyHeight: MapBandState.collapsed ? 0 : Math.max(band.minimumBody,
        Math.min(band.maximumBody, MapBandState.bodyHeight > 0 ? MapBandState.bodyHeight : band.defaultBody))
    // What MonitorPane's SplitView asks of this item.
    readonly property real preferredHeight: MapBandState.collapsed ? band.collapsedHeight
        : band.chromeHeight + band.bodyHeight
    readonly property real minimumHeight: MapBandState.collapsed ? band.collapsedHeight
        : band.chromeHeight + band.minimumBody
    readonly property real maximumHeight: MapBandState.collapsed ? band.collapsedHeight
        : band.chromeHeight + band.maximumBody
    readonly property int locationCount: map.allMembers().length
    readonly property int countryCount: map.hierarchy.available
        ? map.hierarchy.countries.filter(node=>node.code!==null).length : band.locationCount

    // Dragging the band below this much map snaps it collapsed.
    readonly property real collapseBelow: band.minimumBody / 2

    // The splitter handle moved the band to `height`: follow it into the
    // session state, collapsing it when dragged to (nearly) zero.
    function followDrag(height) {
        const body = height - band.chromeHeight;
        MapBandState.collapsed = body < band.collapseBelow;
        if (!MapBandState.collapsed) {
            MapBandState.bodyHeight = Math.min(band.maximumBody, Math.max(band.minimumBody, body));
        }
    }

    // For the headless probes.
    property alias mapView: map
    property alias chevron: chevronButton
    property alias footerText: honestText

    spacing: 0
    clip: true

    RowLayout {
        id: header
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        GuardedToolButton {
            id: chevronButton
            // A text glyph, so it shows without an icon theme (QML cannot
            // tell whether a theme icon will actually render).
            text: MapBandState.collapsed ? "▸" : "▾"
            Accessible.name: MapBandState.collapsed ? "Show map" : "Hide map"
            onClicked: MapBandState.collapsed = !MapBandState.collapsed
            Controls.ToolTip {
                visible: chevronButton.hovered
                contentItem: Controls.Label {
                    textFormat: Text.PlainText
                    color: palette.toolTipText
                    text: MapBandState.collapsed ? "Show map" : "Hide map"
                }
            }
        }
        Controls.Label {
            text: "Map"
            font.bold: true
        }
        Controls.Label {
            Layout.fillWidth: true
            elide: Text.ElideRight
            textFormat: Text.PlainText
            opacity: 0.7
            text: band.locationCount === 0 ? "no destinations yet"
                : band.countryCount + (band.countryCount === 1 ? " country" : " countries")
                    + (map.hierarchy.available ? " · "+map.hierarchy.ips.length+" observed IPs" : "")
        }
        GuardedToolButton {
            objectName: "mapZoomOut"
            text: "−"
            visible: !MapBandState.collapsed
            enabled: map.camera.zoom > map.camera.lowerZoom
            Accessible.name: "Zoom out"
            onClicked: map.camera.zoomAt(1 / 1.5, map.width / 2, map.height / 2)
        }
        GuardedToolButton {
            objectName: "mapZoomIn"
            text: "+"
            visible: !MapBandState.collapsed
            enabled: map.camera.zoom < map.camera.upperZoom
            Accessible.name: "Zoom in"
            onClicked: map.camera.zoomAt(1.5, map.width / 2, map.height / 2)
        }
        GuardedToolButton {
            objectName: "mapFit"
            text: "Fit"
            visible: !MapBandState.collapsed
            Accessible.name: "Fit world map"
            onClicked: map.camera.reset()
        }
        GuardedToolButton {
            id: countriesButton
            text: "Places"
            icon.name: "view-list-details"
            enabled: band.locationCount > 0
            Accessible.description: "Lists countries, local and unknown destinations. Choose one to scope connections while preserving search."
            onClicked: map.openCountryMenu(map.allMembers(), countriesButton)
        }
    }

    RowLayout {
        id: breadcrumbs
        visible: !MapBandState.collapsed
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing
        GuardedToolButton {
            objectName: "mapWorldScope"
            text: "World"
            enabled: map.activeGeoScope.kind !== ""
            Accessible.name: "Clear geographic scope; preserve search"
            onClicked: map.clearScope()
        }
        GuardedToolButton {
            objectName: "mapScopeBack"
            text: "‹ Back"
            enabled: map.activeGeoScope.kind !== ""
            Accessible.name: "Go to enclosing geographic scope"
            onClicked: map.scopeBack()
        }
        Controls.Label {
            Layout.fillWidth: true
            text: map.activeGeoScope.kind === "" ? "Countries → cities → observed IPs" : map.activeGeoScope.name
            Accessible.name: "Geographic scope: " + map.activeGeoScope.name
            textFormat: Text.PlainText
            elide: Text.ElideRight
        }
        GuardedToolButton {
            objectName: "mapDestinations"
            text: "Destinations ("+map.scopeNodes.length+")"
            Accessible.name: "List destinations, including local, unknown and unavailable locations"
            enabled: map.scopeNodes.length>0
            onClicked: map.openCountryMenu(map.scopeNodes,this)
        }
    }

    MapView {
        id: map
        visible: !MapBandState.collapsed
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.minimumHeight: 0
        geoModel: band.geoModel
        connectionsModel: band.connectionsModel
        onActiveGeoScopeChanged: if (band.cameraReady) MapBandState.geoScope=activeGeoScope
    }

    ColumnLayout {
        id: footer
        visible: !MapBandState.collapsed
        Layout.fillWidth: true
        Layout.margins: Kirigami.Units.smallSpacing
        spacing: 0

        Controls.Label {
            Layout.fillWidth: true
            visible: map.hierarchy.available
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            font: Kirigami.Theme.smallFont
            text: "Map: "+(map.hierarchy.snapshot.observedConnections || 0)+" observed cached connections"
                + (band.connectionsModel && band.connectionsModel.matchingCount !== undefined
                   ? "; list: "+band.connectionsModel.matchingCount+" matching connections (search and geographic scope)." : ".")
                + (map.hierarchy.snapshot.capacityReached ? " Geographic coverage is partial: cache capacity reached; remaining rows stay in the unscoped list." : "")
        }
        Flow {
            Layout.fillWidth: true
            spacing: Kirigami.Units.largeSpacing
            Controls.Label {
                text: "● allowed"
                color: Theme.allow
                font: Kirigami.Theme.smallFont
            }
            Controls.Label {
                text: "■ denied"
                color: Theme.deny
                font: Kirigami.Theme.smallFont
            }
            Controls.Label {
                text: "◆ pending"
                color: Theme.pending
                font: Kirigami.Theme.smallFont
            }
            Controls.Label {
                text: "ring: mixed verdicts"
                font: Kirigami.Theme.smallFont
            }
        }
        Controls.Label {
            id: honestText
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            font: Kirigami.Theme.smallFont
            opacity: 0.8
            text: "Markers show approximate database locations of observed destination IPs, not physical endpoints (CDN and anycast addresses can mislead). Coincident IPs share a selectable stack; missing city/coordinates stay in Destinations. Pulses mean a connection event, not data volume. Country shapes: Natural Earth."
        }
    }

    Kirigami.Separator {
        id: separator
        Layout.fillWidth: true
    }
}
