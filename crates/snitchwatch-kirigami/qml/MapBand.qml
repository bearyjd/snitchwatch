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
    readonly property real minimumBody: Kirigami.Units.gridUnit * 6
    readonly property real defaultBody: Kirigami.Units.gridUnit * 13
    // The map is as tall as it is wide / 2.5, so more height is wasted.
    readonly property real maximumBody: Math.max(band.minimumBody,
        band.width > 0 ? band.width / 2.5 : band.defaultBody)
    // The header row and the separator: all that shows when collapsed.
    readonly property real collapsedHeight: header.implicitHeight + separator.implicitHeight
    // Everything but the map itself, when expanded (the footer's margins too).
    readonly property real chromeHeight: band.collapsedHeight
        + footer.implicitHeight + footer.Layout.topMargin + footer.Layout.bottomMargin
    readonly property real bodyHeight: MapBandState.collapsed ? 0 : Math.max(band.minimumBody,
        Math.min(band.maximumBody, MapBandState.bodyHeight > 0 ? MapBandState.bodyHeight : band.defaultBody))
    // What MonitorPane's SplitView asks of this item.
    readonly property real preferredHeight: MapBandState.collapsed ? band.collapsedHeight
        : band.chromeHeight + band.bodyHeight
    readonly property real minimumHeight: MapBandState.collapsed ? band.collapsedHeight
        : band.chromeHeight + band.minimumBody
    readonly property real maximumHeight: MapBandState.collapsed ? band.collapsedHeight
        : band.chromeHeight + band.maximumBody
    readonly property int countryCount: map.allMembers().length

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

        Controls.ToolButton {
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
            text: band.countryCount === 0 ? "no destinations yet"
                : band.countryCount + (band.countryCount === 1 ? " country" : " countries")
        }
        Controls.ToolButton {
            id: countriesButton
            text: "Countries"
            icon.name: "view-list-details"
            enabled: band.countryCount > 0
            Accessible.description: "Lists every country on the map. Choose one to filter the connections to it."
            onClicked: map.openCountryMenu(map.allMembers(), countriesButton)
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
    }

    ColumnLayout {
        id: footer
        visible: !MapBandState.collapsed
        Layout.fillWidth: true
        Layout.margins: Kirigami.Units.smallSpacing
        spacing: 0

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
            text: "Markers show the approximate country of each destination IP (CDN and anycast addresses can mislead). Pulses mean a connection event, not data volume. Country shapes: Natural Earth."
        }
    }

    Kirigami.Separator {
        id: separator
        Layout.fillWidth: true
    }
}
