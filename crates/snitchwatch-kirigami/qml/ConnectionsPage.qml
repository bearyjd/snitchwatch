// Connections list page (Task 8 — view layer + filter/search/inspector).
//
// Binds a ListView to the Rust `ConnectionsModel` (Task 6). Row delegate shows
// process / host:port / protocol with a verdict marker: a hollow ◐ for pending
// rows, allow-green / deny-red for decided ones (per the design spec's
// pending-row styling).
//
// This page adds the Task 8 remainder:
//   * Search/filter — the header SearchField and "pending only" toggle drive
//     `ConnectionsModel.setFilterQuery` / `setPendingOnly`. All filtering logic
//     lives in Rust (`connections::filter`); QML only forwards user input.
//   * Auto-select on new pending row — the model emits `autoSelectRequested`
//     with a visible index; we move `currentIndex` there but do NOT pop the
//     inspector, so it never steals interaction focus from the user.
//   * Inspector — a Kirigami.OverlaySheet opened by clicking a row (design
//     decision: OverlaySheet over a SplitView detail column, chosen because it
//     works identically on narrow and wide windows and matches the pending-row
//     inspector described in the design spec without a responsive-layout branch).
//
// Little-Snitch-parity grouping (Process -> Domain -> connection): the
// header's "Grouped" switch drives `ConnectionsModel.setGrouped`. In grouped
// mode the same `ListView`/delegate renders a mix of row kinds — process
// headers (depth 0), domain headers (depth 1), and leaf connection rows
// (depth 2) — distinguished by the `isGroupHeader`/`depth` roles the Rust
// model exposes; clicking a header toggles its expand state via
// `toggleProcessGroup`/`toggleDomainGroup` instead of opening the inspector.
// All grouping/aggregate logic lives in Rust (`connections::grouping`); this
// QML only renders the flattened projection and forwards toggle clicks.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Connections"

    // Injected by the caller (main.qml) so the model's lifetime is owned there.
    property ConnectionsModel model

    // Live-wiring hub (Task 13), injected by main.qml. Threaded down to the
    // embedded PendingDecisionSheet so a submitted verdict reaches the bridge's
    // inbound pump. Null in isolated component tests (the sheet no-ops then).
    property var bridgeFeed: null

    // Snapshot of the row currently shown in the inspector sheet.
    property string inspectId: ""
    property string inspectProcess: ""
    property string inspectHost: ""
    property int inspectPort: 0
    property string inspectProtocol: ""
    property string inspectVerdict: ""
    property bool inspectPending: false

    // Verdict token -> accent colour. Kept in QML since it's pure presentation.
    function verdictColor(verdict) {
        switch (verdict) {
        case "pending": return Kirigami.Theme.neutralTextColor;
        case "allowed": return Kirigami.Theme.positiveTextColor;
        case "denied": return Kirigami.Theme.negativeTextColor;
        case "blocklisted": return Kirigami.Theme.negativeTextColor;
        default: return Kirigami.Theme.disabledTextColor;
        }
    }

    function verdictGlyph(verdict, pending) {
        if (pending) return "◐";
        switch (verdict) {
        case "allowed": return "●";
        case "denied": return "●";
        case "blocklisted": return "⊘";
        default: return "○";
        }
    }

    // React to the Rust auto-select policy: move the selection, but never open
    // the inspector — surfacing a pending row must not grab the user's focus.
    Connections {
        target: page.model
        function onAutoSelectRequested(row) {
            list.currentIndex = row;
        }
    }

    // Search + pending-only filter live in the page header so they stay visible
    // while the list scrolls.
    titleDelegate: RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Heading {
            text: page.title
            level: 1
            Layout.alignment: Qt.AlignVCenter
        }
        Kirigami.SearchField {
            id: search
            Layout.fillWidth: true
            placeholderText: "Filter by process, host, port…"
            onTextChanged: page.model.setFilterQuery(text)
        }
        Controls.Switch {
            id: pendingOnly
            text: "Pending only"
            onToggled: page.model.setPendingOnly(checked)
        }
        Controls.Switch {
            id: grouped
            text: "Grouped"
            checked: page.model ? page.model.grouped : true
            onToggled: page.model.setGrouped(checked)
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: (page.model && page.model.totalCount > 0) ? "search" : "network-connect"
        text: (page.model && page.model.totalCount > 0)
              ? "No matching connections"
              : "No connections yet"
        explanation: (page.model && page.model.totalCount > 0)
              ? "No rows match the current filter."
              : "New connection prompts and recent decisions will appear here."
    }

    ListView {
        id: list
        model: page.model
        currentIndex: -1
        reuseItems: true

        // Keep the Rust model's notion of the current selection in sync so its
        // auto-select policy knows whether the user is investigating a row.
        onCurrentIndexChanged: {
            const item = list.itemAtIndex(list.currentIndex);
            page.model.setCurrentRowId(item ? item.rowId : "");
        }

        delegate: Controls.ItemDelegate {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            highlighted: ListView.isCurrentItem

            required property int index
            required property string rowId
            required property string process
            required property string host
            required property int port
            required property string protocol
            required property string verdict
            required property bool pending

            // Grouping roles (Little-Snitch-parity Process->Domain view).
            // depth: 0 = process header, 1 = domain header, 2 = leaf
            // connection row. Flat mode always reports depth 0 / isGroupHeader
            // false, so this delegate renders identically to before grouping
            // was added.
            required property int depth
            required property bool isGroupHeader
            required property bool expanded
            required property string groupKey
            required property string groupParentKey
            required property string groupLabel
            required property int groupTotal
            required property int groupPending
            required property int groupAllowed
            required property int groupDenied
            required property int groupBlocklisted

            onClicked: {
                if (row.isGroupHeader) {
                    if (row.depth === 0) {
                        page.model.toggleProcessGroup(row.groupKey);
                    } else {
                        page.model.toggleDomainGroup(row.groupParentKey, row.groupKey);
                    }
                    return;
                }
                list.currentIndex = row.index;
                page.openInspector(row);
            }

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                Item {
                    // Indent nested rows/headers under their ancestors.
                    Layout.preferredWidth: row.depth * Kirigami.Units.gridUnit
                }

                Kirigami.Icon {
                    visible: row.isGroupHeader
                    source: row.expanded ? "arrow-down" : "arrow-right"
                    Layout.preferredWidth: Kirigami.Units.iconSizes.small
                    Layout.preferredHeight: Kirigami.Units.iconSizes.small
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    visible: !row.isGroupHeader
                    text: page.verdictGlyph(row.verdict, row.pending)
                    color: page.verdictColor(row.verdict)
                    Layout.alignment: Qt.AlignVCenter
                }

                ColumnLayout {
                    visible: !row.isGroupHeader
                    Layout.fillWidth: true
                    spacing: 0
                    Controls.Label {
                        text: row.process
                        font.bold: row.pending
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        text: row.host + ":" + row.port + "  " + row.protocol
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                }

                Controls.Label {
                    visible: !row.isGroupHeader
                    text: row.pending ? "pending" : row.verdict
                    color: page.verdictColor(row.verdict)
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    visible: row.isGroupHeader
                    text: row.groupLabel
                    font.bold: true
                    color: row.groupPending > 0 ? Kirigami.Theme.neutralTextColor : Kirigami.Theme.textColor
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }

                Controls.Label {
                    visible: row.isGroupHeader
                    text: row.groupPending > 0
                          ? (row.groupTotal + " · " + row.groupPending + " pending")
                          : (row.groupTotal + " connection" + (row.groupTotal === 1 ? "" : "s"))
                    color: row.groupPending > 0 ? Kirigami.Theme.neutralTextColor : Kirigami.Theme.disabledTextColor
                    Layout.alignment: Qt.AlignVCenter
                }
            }
        }
    }

    function openInspector(row) {
        page.inspectId = row.rowId;
        page.inspectProcess = row.process;
        page.inspectHost = row.host;
        page.inspectPort = row.port;
        page.inspectProtocol = row.protocol;
        page.inspectVerdict = row.verdict;
        page.inspectPending = row.pending;
        inspector.open();
    }

    // Row inspector. For a *pending* row this is where the decision prompt lives
    // (Task 7 wires the verdict actions in); for decided rows it is read-only
    // detail. Kept as an OverlaySheet so it behaves the same at every width.
    Kirigami.OverlaySheet {
        id: inspector
        title: page.inspectProcess

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            Kirigami.FormLayout {
                Layout.fillWidth: true
                Controls.Label {
                    Kirigami.FormData.label: "Host"
                    text: page.inspectHost
                }
                Controls.Label {
                    Kirigami.FormData.label: "Port"
                    text: page.inspectPort
                }
                Controls.Label {
                    Kirigami.FormData.label: "Protocol"
                    text: page.inspectProtocol
                }
                Controls.Label {
                    Kirigami.FormData.label: "Verdict"
                    text: page.inspectPending ? "pending" : page.inspectVerdict
                    color: page.verdictColor(page.inspectVerdict)
                }
            }

            // Pending decision surface (Task 7). The countdown/timeout stays
            // server-side; these buttons call the bridge's verdict path once
            // pending_decision.rs is wired to the injected model.
            PendingDecisionSheet {
                Layout.fillWidth: true
                visible: page.inspectPending
                rowId: page.inspectId
                process: page.inspectProcess
                host: page.inspectHost
                bridgeFeed: page.bridgeFeed
                onDecided: inspector.close()
            }
        }
    }
}
