// A group header's verdict counts for the last hour and today (phase 0b): an
// icon and a word for every verdict, never colour alone. The model sends each
// window as a JSON list of {kind, text} (`groupLastHour`, `groupToday`); the
// figures cover only the connections Snitchwatch still holds (see `note`).
// Sits on its own line along the bottom of the ConnectionRow `src`, so it
// never squeezes the header's label or buttons; zero height on connections.
//
// Each icon+count pair is a capsule chip (RoundedSurface, radiusPill). The
// chips are NOT independently tappable: they sit on the clickable group-header
// row, which is the hit target (its hit area is at least Theme.hitMin), so
// they stay small on purpose, take no focus and add no Tab stop.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: root
    objectName: "groupVerdictCounts"

    // The ConnectionRow this belongs to (its parent).
    property var src
    property string lastHour: src ? src.groupLastHour : "[]"
    property string today: src ? src.groupToday : "[]"
    visible: !!src && src.isGroupHeader
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: parent.bottom
    anchors.leftMargin: Kirigami.Units.smallSpacing
        + (src ? src.depth + 1 : 0) * Kirigami.Units.gridUnit
    anchors.rightMargin: Kirigami.Units.largeSpacing
    anchors.bottomMargin: Kirigami.Units.smallSpacing
    height: visible ? flow.implicitHeight : 0
    z: 1
    // Bridge side: a reconnect replays the bridge's in-memory cache (it evicts
    // its oldest rows past a configured size); a bridge restart empties it.
    readonly property string note: "Counted by when each connection started, from the "
        + "connections Snitchwatch still holds, not a full record of the hour or the day. "
        + "After Snitchwatch reconnects, the bridge resends only the connections it still holds; "
        + "after the bridge restarts, these counts start from zero. "
        + "Repeats shown as one row count once."
    readonly property var windows: [
        { label: "Last hour", segments: root.parse(root.lastHour) },
        { label: "Today", segments: root.parse(root.today) }
    ]
    // The same figures as one sentence per window, for screen readers.
    readonly property string summary: root.windows.map(w => w.label + ": "
        + (w.segments.length > 0 ? w.segments.map(s => s.text).join(" · ") : "no connections"))
        .join(". ")

    Accessible.role: Accessible.StaticText
    Accessible.name: root.summary

    function parse(json) {
        try {
            const v = JSON.parse(json);
            return Array.isArray(v) ? v : [];
        } catch (e) {
            return [];
        }
    }

    function iconFor(kind) {
        switch (kind) {
        case "allowed": return "dialog-ok-apply";
        case "denied": return "edit-delete-remove";
        case "blocklisted": return "action-unavailable";
        case "pending": return "chronometer";
        default: return "dialog-question";
        }
    }

    // Every icon+word pair is its own Flow child (a window's label rides on
    // its first pair), so a narrow pane wraps between pairs instead of
    // clipping a window's line; a pair wider than the pane elides.
    readonly property var pieces: root.windows.reduce((out, w) => out.concat(
        (w.segments.length > 0 ? w.segments : [{ kind: "", text: "no connections" }])
            .map((s, i) => ({ label: i === 0 ? w.label + ":" : "", kind: s.kind, text: s.text }))),
        [])

    Flow {
        id: flow
        width: root.width
        spacing: Kirigami.Units.smallSpacing

        Repeater {
            model: root.pieces
            delegate: Row {
                id: piece
                required property var modelData
                required property int index
                spacing: 2
                // A gap before each window's label but the first.
                leftPadding: piece.modelData.label === "" || piece.index === 0 ? 0 : Kirigami.Units.largeSpacing

                Controls.Label {
                    id: windowLabel
                    objectName: "verdictWindowLabel"
                    visible: piece.modelData.label !== ""
                    textFormat: Text.PlainText
                    text: piece.modelData.label
                    rightPadding: Kirigami.Units.smallSpacing
                    font: Kirigami.Theme.smallFont
                    opacity: 0.7
                }
                Controls.Control {
                    id: chip
                    // Only a verdict pair is a chip; "no connections" is plain text.
                    readonly property bool framed: piece.modelData.kind !== ""
                    horizontalPadding: chip.framed ? Kirigami.Units.smallSpacing : 0
                    focusPolicy: Qt.NoFocus
                    background: RoundedSurface {
                        control: chip
                        visible: chip.framed
                        implicitWidth: 0
                        implicitHeight: 0
                    }
                    contentItem: Row {
                        spacing: piece.spacing
                        Kirigami.Icon {
                            id: pieceIcon
                            visible: piece.modelData.kind !== ""
                            source: root.iconFor(piece.modelData.kind)
                            width: Kirigami.Units.iconSizes.small
                            height: Kirigami.Units.iconSizes.small
                        }
                        Controls.Label {
                            objectName: "verdictCount"
                            textFormat: Text.PlainText
                            text: piece.modelData.text
                            font: Kirigami.Theme.smallFont
                            opacity: piece.modelData.kind === "" ? 0.7 : 1
                            elide: Text.ElideRight
                            width: Math.min(implicitWidth, Math.max(0, flow.width - piece.leftPadding
                                - (windowLabel.visible ? windowLabel.width + piece.spacing : 0)
                                - chip.leftPadding - chip.rightPadding
                                - (pieceIcon.visible ? pieceIcon.width + piece.spacing : 0)))
                        }
                    }
                }
            }
        }
    }

    HoverTip {
        text: root.note
    }
}
