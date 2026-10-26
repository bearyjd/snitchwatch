// A group header's verdict counts for the last hour and today (phase 0b): an
// icon and a word for every verdict, never colour alone. The model sends each
// window as a JSON list of {kind, text} (`groupLastHour`, `groupToday`); the
// figures cover only the connections Snitchwatch still holds (see `note`).
// Sits on its own line along the bottom of the ConnectionRow `src`, so it
// never squeezes the header's label or buttons; zero height on connections.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Item {
    id: root

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
    // Bridge side: a reconnect replays the bridge's in-memory cache (at most
    // 10,000 rows, oldest evicted first); a bridge restart empties it.
    readonly property string note: "Counted by when each connection started, from the "
        + "connections Snitchwatch still holds, not a full record of the hour or the day. "
        + "After Snitchwatch reconnects, the bridge resends at most its 10,000 most recent "
        + "connections; after the bridge restarts, these counts start from zero. "
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

    Flow {
        id: flow
        width: root.width
        spacing: Kirigami.Units.largeSpacing

        Repeater {
            model: root.windows
            delegate: RowLayout {
                id: window
                required property var modelData
                spacing: Kirigami.Units.smallSpacing

                Controls.Label {
                    textFormat: Text.PlainText
                    text: window.modelData.label + ":"
                    font: Kirigami.Theme.smallFont
                    opacity: 0.7
                }
                Controls.Label {
                    objectName: "verdictCount"
                    visible: window.modelData.segments.length === 0
                    textFormat: Text.PlainText
                    text: "no connections"
                    font: Kirigami.Theme.smallFont
                    opacity: 0.7
                }
                Repeater {
                    model: window.modelData.segments
                    delegate: RowLayout {
                        id: segment
                        required property var modelData
                        spacing: 2

                        Kirigami.Icon {
                            source: root.iconFor(segment.modelData.kind)
                            Layout.preferredWidth: Kirigami.Units.iconSizes.small
                            Layout.preferredHeight: Kirigami.Units.iconSizes.small
                        }
                        Controls.Label {
                            objectName: "verdictCount"
                            textFormat: Text.PlainText
                            text: segment.modelData.text
                            font: Kirigami.Theme.smallFont
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
