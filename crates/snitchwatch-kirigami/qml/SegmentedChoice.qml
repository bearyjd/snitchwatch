// A row of mutually exclusive buttons ("segments"): the alert's scope and
// duration choosers. `model` is a list of `{ label, token }`; the chosen
// token is `currentToken`, which falls back to the first entry when the
// model no longer holds it (`effectiveToken`) — a shrunken duration list
// then reads "Once" without forgetting what the user picked.
//
// Drawn as one joined capsule (iOS / Little Snitch 6 look): a shared soft
// fill and hairline (`track`), hairline dividers, the chosen segment filled
// with the palette's selection colour. Only the outer corners are round, so
// the first and last segment swap corners in a right-to-left layout. Each
// segment is restyled in place (it stays a `Controls.Button`, the name the
// keyboard probes read) and is at least `Theme.hitMin` high. The visuals are
// role-less and unfocusable: no extra Tab stop, no input of their own.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import QtQuick.Controls.impl as Impl
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Item {
    id: seg

    property var model: []
    property string currentToken: ""
    // Names the group for a screen reader: "<groupName>: <label>".
    property string groupName: ""
    // Every key pressed or released on a choice, for the owner's held-key
    // bookkeeping (a choice consumes Return and a native Space).
    signal keyDown(int key, bool autoRepeat)
    signal keyUp(int key, bool autoRepeat)
    readonly property string effectiveToken: seg.indexOfToken(seg.currentToken) >= 0
        ? seg.currentToken : (seg.model.length > 0 ? seg.model[0].token : "")
    // Exposed for the headless probe (tests/restyle_batch_b_qml.rs).
    readonly property alias track: track

    implicitWidth: strip.implicitWidth
    implicitHeight: strip.implicitHeight

    function indexOfToken(token) {
        for (let i = 0; i < seg.model.length; i++) {
            if (seg.model[i].token === token) {
                return i;
            }
        }
        return -1;
    }

    // The index of the choice button `item`, or -1.
    function indexOfItem(item) {
        for (let i = 0; i < rep.count; i++) {
            if (rep.itemAt(i) === item) {
                return i;
            }
        }
        return -1;
    }

    // Focus the choice button at `index`, or the last one if there are fewer.
    function focusAt(index) {
        const item = rep.itemAt(Math.min(index, rep.count - 1));
        if (item !== null) {
            item.forceActiveFocus();
        }
    }

    // The shared outline under every segment.
    Rectangle {
        id: track
        objectName: "segmentTrack"
        anchors.fill: strip
        radius: height / 2
        opacity: seg.enabled ? 1 : Theme.disabledOpacity
        color: Qt.alpha(seg.palette.windowText, Theme.fillOpacity)
        border.width: Theme.borderWidth
        border.color: Qt.alpha(seg.palette.windowText, Theme.borderOpacity)
    }

    RowLayout {
        id: strip
        anchors.fill: parent
        spacing: 0

        Repeater {
            id: rep
            model: seg.model
            delegate: Controls.Button {
                id: segment
                required property var modelData
                required property int index
                // Which outer corners are round: the visual left and right ends.
                readonly property bool atLeft: segment.mirrored ? segment.index === rep.count - 1 : segment.index === 0
                readonly property bool atRight: segment.mirrored ? segment.index === 0 : segment.index === rep.count - 1
                // The segment drawn left of this one is the chosen one.
                readonly property bool leftOfChosen: {
                    const left = segment.index + (segment.mirrored ? 1 : -1);
                    return left >= 0 && left < seg.model.length && seg.model[left].token === seg.effectiveToken;
                }
                Layout.fillWidth: true
                Layout.preferredWidth: 1
                // Restyled in place (type name stays `Button` for the focus tests).
                implicitHeight: Math.max(Theme.hitMin, implicitContentHeight + topPadding + bottomPadding)
                horizontalPadding: Theme.controlPaddingH / 2
                verticalPadding: Kirigami.Units.smallSpacing
                text: modelData.label
                highlighted: modelData.token === seg.effectiveToken
                Accessible.name: seg.groupName !== "" ? seg.groupName + ": " + modelData.label : modelData.label
                Accessible.checked: highlighted
                onClicked: seg.currentToken = modelData.token
                // Q01: Return on a focused choice only selects it, as a click
                // does; it never goes on to the window's Return = Allow.
                Keys.onReleased: event => seg.keyUp(event.key, event.isAutoRepeat)
                Keys.onPressed: event => {
                    // Only keys the choice consumes; the rest reach the owner's
                    // own handler, which records them.
                    if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter || event.key === Qt.Key_Space) {
                        seg.keyDown(event.key, event.isAutoRepeat);
                    }
                    if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                        event.accepted = true;
                        if (!event.isAutoRepeat) {
                            seg.currentToken = modelData.token;
                        }
                    }
                }

                contentItem: Impl.IconLabel {
                    spacing: segment.spacing
                    mirrored: segment.mirrored
                    display: segment.display
                    icon: segment.icon
                    text: segment.text
                    font: segment.font
                    color: segment.highlighted ? segment.palette.highlightedText : segment.palette.buttonText
                }

                // No implicit size of its own: the clamp above is the height.
                background: Item {
                    id: surface
                    objectName: "segmentSurface"
                    readonly property real leftRadius: segment.atLeft ? height / 2 : 0
                    readonly property real rightRadius: segment.atRight ? height / 2 : 0
                    readonly property color ink: segment.highlighted ? segment.palette.highlightedText
                        : segment.palette.windowText
                    readonly property alias body: body
                    readonly property alias focusRing: ring
                    readonly property alias wash: wash
                    readonly property alias divider: divider
                    opacity: segment.enabled ? 1 : Theme.disabledOpacity

                    // The chosen segment's fill.
                    Rectangle {
                        id: body
                        anchors.fill: parent
                        visible: segment.highlighted
                        color: segment.palette.highlight
                        topLeftRadius: surface.leftRadius
                        bottomLeftRadius: surface.leftRadius
                        topRightRadius: surface.rightRadius
                        bottomRightRadius: surface.rightRadius
                    }
                    // Hairline on the left edge, between two unchosen segments.
                    Rectangle {
                        id: divider
                        objectName: "segmentDivider"
                        x: 0
                        width: Theme.separatorWidth
                        height: parent.height
                        visible: !segment.atLeft && !segment.highlighted && !segment.leftOfChosen
                        color: Qt.alpha(seg.palette.windowText, Theme.borderOpacity)
                    }
                    // Hover and pressed wash.
                    Rectangle {
                        id: wash
                        anchors.fill: parent
                        color: surface.ink
                        opacity: segment.down ? Theme.pressedOpacity
                            : segment.hovered ? Theme.hoverOpacity : 0
                        topLeftRadius: surface.leftRadius
                        bottomLeftRadius: surface.leftRadius
                        topRightRadius: surface.rightRadius
                        bottomRightRadius: surface.rightRadius
                    }
                    // Keyboard focus ring, inside the bounds; on the chosen
                    // fill it takes the text colour, so it never matches it.
                    Rectangle {
                        id: ring
                        objectName: "focusRing"
                        anchors.fill: parent
                        color: "transparent"
                        visible: segment.visualFocus
                        border.width: Theme.focusRingWidth
                        border.color: segment.highlighted ? segment.palette.highlightedText
                            : segment.palette.highlight
                        topLeftRadius: surface.leftRadius
                        bottomLeftRadius: surface.leftRadius
                        topRightRadius: surface.rightRadius
                        bottomRightRadius: surface.rightRadius
                    }
                }
            }
        }
    }
}
