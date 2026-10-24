// One marker (a country, or a cluster of nearby countries) on the map.
//
// Verdict is never colour alone: allowed is a green circle with a check,
// denied a red square with a cross, pending an orange diamond with a half
// circle; a mixed marker also gets a ring. Size scales with log(total).
// `cluster` is one entry of GeoModel.clustersJson (see geo/map_markers.rs).
import QtQuick
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Controls.AbstractButton {
    id: marker

    property var cluster: ({ members: [], total: 0, dominant: "none", mixed: false, summary: "" })
    property bool reducedMotion: false
    property bool highlighted: false
    property real labelWidth: 160
    // At most one pulse per marker in this window.
    property int pulseIntervalMs: 500
    // Pulses actually started (probes read this).
    property int pulseCount: 0
    property double lastPulseMs: -1e12

    readonly property string dominant: cluster.dominant
    readonly property bool isCluster: cluster.members.length > 1
    readonly property real diameter: Math.min(Kirigami.Units.gridUnit * 2.2,
        Kirigami.Units.gridUnit * 0.9 + Kirigami.Units.gridUnit * 0.35 * Math.log(1 + cluster.total) / Math.LN2)
    readonly property color verdictColor: dominant === "denied" ? Theme.deny
        : dominant === "pending" ? Theme.pending
        : dominant === "allowed" ? Theme.allow : Theme.partial
    readonly property string glyph: isCluster ? String(cluster.members.length)
        : dominant === "denied" ? "×" : dominant === "pending" ? "◐" : dominant === "allowed" ? "✓" : ""

    // Name carries the counts in words, so nothing depends on the colour.
    Accessible.name: cluster.summary
    Accessible.role: Accessible.Button
    hoverEnabled: true
    activeFocusOnTab: true
    padding: 0
    width: diameter + Kirigami.Units.smallSpacing * 2
    height: width

    // Start a pulse unless one ran within the last `pulseIntervalMs`, or the
    // user asked for reduced motion. `nowMs` is injectable for tests.
    function pulse(nowMs) {
        if (marker.reducedMotion || nowMs - marker.lastPulseMs < marker.pulseIntervalMs) {
            return false;
        }
        marker.lastPulseMs = nowMs;
        marker.pulseCount++;
        pulseAnim.restart();
        return true;
    }

    background: Item {
        // Focus / hover halo.
        Rectangle {
            anchors.centerIn: parent
            width: marker.diameter + Kirigami.Units.smallSpacing * 2
            height: width
            radius: width / 2
            visible: marker.visualFocus || marker.hovered || marker.highlighted
            color: "transparent"
            border.width: 2
            border.color: Kirigami.Theme.highlightColor
        }
        // The pulse: an expanding ring, "a connection event happened".
        Rectangle {
            id: pulseRing
            anchors.centerIn: parent
            width: marker.diameter
            height: width
            radius: width / 2
            color: "transparent"
            border.width: 2
            border.color: marker.verdictColor
            opacity: 0
            scale: 1
            ParallelAnimation {
                id: pulseAnim
                NumberAnimation { target: pulseRing; property: "scale"; from: 1; to: 2.2; duration: Kirigami.Units.veryLongDuration * 1.5 }
                NumberAnimation { target: pulseRing; property: "opacity"; from: 0.9; to: 0; duration: Kirigami.Units.veryLongDuration * 1.5 }
            }
        }
        // Mixed verdicts: an outer ring around the dominant shape.
        Rectangle {
            anchors.centerIn: parent
            width: marker.diameter + Kirigami.Units.smallSpacing * 1.5
            height: width
            radius: marker.dominant === "denied" ? 3 : width / 2
            visible: marker.cluster.mixed
            color: "transparent"
            border.width: 2
            border.color: Kirigami.Theme.textColor
        }
        // The dominant shape: circle / square / diamond.
        Rectangle {
            anchors.centerIn: parent
            width: marker.dominant === "pending" ? marker.diameter * 0.74 : marker.diameter
            height: width
            radius: marker.dominant === "denied" ? 2
                : marker.dominant === "pending" ? 1 : width / 2
            rotation: marker.dominant === "pending" ? 45 : 0
            color: marker.verdictColor
        }
    }

    contentItem: Item {
        Controls.Label {
            anchors.centerIn: parent
            text: marker.glyph
            width: marker.diameter
            height: marker.diameter
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            fontSizeMode: Text.Fit
            minimumPixelSize: 8
            textFormat: Text.PlainText
            font.bold: true
            font.pixelSize: Math.round(marker.diameter * 0.6)
            color: Kirigami.Theme.backgroundColor
        }
    }

    Controls.Label {
        anchors.top: parent.bottom
        anchors.horizontalCenter: parent.horizontalCenter
        width: marker.labelWidth
        horizontalAlignment: Text.AlignHCenter
        elide: Text.ElideRight
        visible: marker.labelWidth>=24 && marker.cluster.members.length===1 && marker.cluster.members[0].kind !== undefined
        text: visible ? marker.cluster.members[0].name : ""
        textFormat: Text.PlainText
        font: Kirigami.Theme.smallFont
        background: Rectangle { color: Kirigami.Theme.backgroundColor; opacity:0.85;radius:2 }
    }

    // Country names come from the GeoIP file, so the tip is an explicit
    // PlainText label (issue #51), never the style's AutoText one.
    Controls.ToolTip {
        visible: marker.hovered || marker.visualFocus
        contentItem: Controls.Label {
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            color: palette.toolTipText
            text: marker.cluster.summary
        }
    }
}
