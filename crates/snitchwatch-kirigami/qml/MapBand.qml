// The featured map band above the connections tree (LS redesign phase 2,
// step 9): a header with a collapse chevron, the map, a legend, the honest
// footer, and a drag handle to resize it.
//
// Visible by default. Collapse and height live in MapBandState, a session-only
// singleton (never in settings). On a short window the band shrinks to its
// minimum height before the tree below it does: the page passes its own
// height as `availableHeight` and the tree keeps `treeMinimum` of it.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: band

    property var geoModel: null
    property var connectionsModel: null
    // Height of the page the band sits in; 0 = unconstrained.
    property real availableHeight: 0
    readonly property real minimumBody: Kirigami.Units.gridUnit * 6
    readonly property real defaultBody: Kirigami.Units.gridUnit * 13
    readonly property real treeMinimum: Kirigami.Units.gridUnit * 16
    readonly property real chromeHeight: header.height + footer.height + grip.height
    readonly property real maximumBody: Math.max(band.minimumBody, Math.min(
        band.width > 0 ? band.width / 2.5 : band.defaultBody,
        band.availableHeight > 0 ? band.availableHeight - band.treeMinimum - band.chromeHeight
                                 : band.defaultBody * 4))
    readonly property real bodyHeight: MapBandState.collapsed ? 0 : Math.max(band.minimumBody,
        Math.min(band.maximumBody, MapBandState.bodyHeight > 0 ? MapBandState.bodyHeight : band.defaultBody))
    readonly property int countryCount: map.allMembers().length

    // For the headless probes.
    property alias mapView: map
    property alias chevron: chevronButton
    property alias footerText: honestText
    property alias gripHandle: grip

    spacing: 0

    RowLayout {
        id: header
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        Controls.ToolButton {
            id: chevronButton
            // A text glyph, so it shows without an icon theme.
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
        Layout.preferredHeight: band.bodyHeight
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

    // Drag (or Up/Down when focused) to resize the map.
    Rectangle {
        id: grip
        visible: !MapBandState.collapsed
        Layout.fillWidth: true
        Layout.preferredHeight: Kirigami.Units.gridUnit * 0.7
        color: gripDrag.active || activeFocus ? Kirigami.Theme.highlightColor : "transparent"
        activeFocusOnTab: true
        Accessible.name: "Resize map"
        Accessible.role: Accessible.Slider
        property real startHeight: 0
        Rectangle {
            anchors.centerIn: parent
            width: Kirigami.Units.gridUnit * 3
            height: 3
            radius: 1.5
            color: Kirigami.Theme.textColor
            opacity: 0.4
        }
        HoverHandler { cursorShape: Qt.SizeVerCursor }
        DragHandler {
            id: gripDrag
            target: null
            xAxis.enabled: false
            onActiveChanged: if (active) grip.startHeight = band.bodyHeight
            onActiveTranslationChanged: if (active) {
                MapBandState.bodyHeight = Math.max(band.minimumBody,
                    Math.min(band.maximumBody, grip.startHeight + activeTranslation.y));
            }
        }
        Keys.onUpPressed: MapBandState.bodyHeight = Math.max(band.minimumBody, band.bodyHeight - Kirigami.Units.gridUnit)
        Keys.onDownPressed: MapBandState.bodyHeight = Math.min(band.maximumBody, band.bodyHeight + Kirigami.Units.gridUnit)
    }

    Kirigami.Separator {
        Layout.fillWidth: true
    }
}
