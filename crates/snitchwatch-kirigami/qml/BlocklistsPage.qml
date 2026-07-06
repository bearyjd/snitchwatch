// Blocklists tab (Task 9 — view layer over BlocklistsModel/BlocklistEntriesModel).
//
// Same list/detail shape as ConnectionsPage.qml (Task 8): a ListView of
// subscriptions bound to `BlocklistsModel`, with a Kirigami.OverlaySheet detail
// view (reusing the OverlaySheet inspector pattern established there) showing
// the per-subscription entry (host) list bound to `BlocklistEntriesModel`.
//
// Subscribe/unsubscribe are plain qinvokables on `BlocklistsModel`
// (`subscribe(url)` / `unsubscribe(id)`); they emit `subscriptionRequested`
// with a JSON-encoded `ClientMessage` for the live bridge feed to forward, the
// same signal-out pattern `PendingDecision.submit` uses for verdicts — no
// bridge changes, and consuming that signal into the bridge's live request
// path is the same kind of consumer-side follow-up already noted for
// `ConnectionsModel`'s and `PendingDecision`'s live wiring.
//
// Status display (last-updated, fetch-failed) reads the bridge's `FetchStatus`
// as already projected into `BlocklistsModel`'s `status` / `lastUpdated` /
// `lastFailureReason` roles by the Rust row store — no fetch-status logic
// lives in QML.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

Kirigami.ScrollablePage {
    id: page
    title: "Blocklists"

    // Injected by the caller (main.qml) so the models' lifetime is owned there.
    property BlocklistsModel model
    property BlocklistEntriesModel entriesModel

    // Snapshot of the subscription currently shown in the detail sheet.
    property string inspectId: ""
    property string inspectDisplayName: ""
    property string inspectUrl: ""
    property int inspectEntryCount: 0
    property string inspectStatus: ""
    property string inspectLastUpdated: ""
    property string inspectLastFailureReason: ""

    function statusColor(status) {
        switch (status) {
        case "ok": return Kirigami.Theme.positiveTextColor;
        case "failed": return Kirigami.Theme.negativeTextColor;
        default: return Kirigami.Theme.neutralTextColor;
        }
    }

    // Subscribe box lives in the page header so it stays visible while the
    // list scrolls, mirroring ConnectionsPage's search field placement.
    titleDelegate: RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Heading {
            text: page.title
            level: 1
            Layout.alignment: Qt.AlignVCenter
        }
        Controls.TextField {
            id: subscribeUrl
            Layout.fillWidth: true
            placeholderText: "Blocklist URL to subscribe…"
        }
        Controls.Button {
            text: "Subscribe"
            icon.name: "list-add"
            enabled: subscribeUrl.text.trim().length > 0
            onClicked: {
                page.model.subscribe(subscribeUrl.text.trim());
                subscribeUrl.text = "";
            }
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: "edit-delete"
        text: "No blocklist subscriptions yet"
        explanation: "Subscribe to a blocklist URL above to start filtering hosts."
    }

    ListView {
        id: list
        model: page.model
        currentIndex: -1
        reuseItems: true

        delegate: Controls.ItemDelegate {
            id: row
            width: ListView.view ? ListView.view.width : implicitWidth
            highlighted: ListView.isCurrentItem

            required property int index
            required property string listId
            required property string displayName
            required property string url
            required property int entryCount
            required property string status
            required property string lastUpdated
            required property string lastFailureReason

            onClicked: {
                list.currentIndex = row.index;
                page.openInspector(row);
            }

            contentItem: RowLayout {
                spacing: Kirigami.Units.largeSpacing

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 0
                    Controls.Label {
                        text: row.displayName
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        text: row.url
                        opacity: 0.7
                        font: Kirigami.Theme.smallFont
                        elide: Text.ElideMiddle
                        Layout.fillWidth: true
                    }
                }

                Controls.Label {
                    text: row.entryCount + " hosts"
                    opacity: 0.7
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    text: row.status
                    color: page.statusColor(row.status)
                    Layout.alignment: Qt.AlignVCenter
                }
            }
        }
    }

    function openInspector(row) {
        page.inspectId = row.listId;
        page.inspectDisplayName = row.displayName;
        page.inspectUrl = row.url;
        page.inspectEntryCount = row.entryCount;
        page.inspectStatus = row.status;
        page.inspectLastUpdated = row.lastUpdated;
        page.inspectLastFailureReason = row.lastFailureReason;
        inspector.open();
    }

    // Subscription detail + entries. Kept as an OverlaySheet, same as
    // ConnectionsPage's inspector, so it behaves identically at every width.
    Kirigami.OverlaySheet {
        id: inspector
        title: page.inspectDisplayName

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            Kirigami.FormLayout {
                Layout.fillWidth: true
                Controls.Label {
                    Kirigami.FormData.label: "URL"
                    text: page.inspectUrl
                    elide: Text.ElideMiddle
                }
                Controls.Label {
                    Kirigami.FormData.label: "Entries"
                    text: page.inspectEntryCount
                }
                Controls.Label {
                    Kirigami.FormData.label: "Status"
                    text: page.inspectStatus
                    color: page.statusColor(page.inspectStatus)
                }
                Controls.Label {
                    Kirigami.FormData.label: "Last updated"
                    visible: page.inspectLastUpdated.length > 0
                    text: page.inspectLastUpdated
                }
                Controls.Label {
                    Kirigami.FormData.label: "Last failure"
                    visible: page.inspectStatus === "failed" && page.inspectLastFailureReason.length > 0
                    text: page.inspectLastFailureReason
                    color: Kirigami.Theme.negativeTextColor
                    wrapMode: Text.Wrap
                }
            }

            Kirigami.Separator {
                Layout.fillWidth: true
            }

            Kirigami.Heading {
                level: 3
                text: "Entries"
            }

            // The entries model holds at most one subscription's hosts at a
            // time (fed by the live bridge feed's SetBlocklistEntries
            // message). Show them when they match what's being inspected;
            // otherwise this subscription's entries haven't been pushed yet.
            Kirigami.PlaceholderMessage {
                Layout.fillWidth: true
                visible: !page.entriesModel || page.entriesModel.subscriptionId !== page.inspectId
                icon.name: "view-refresh"
                text: "Entries not loaded"
                explanation: "Waiting for the live feed to push this subscription's host list."
            }

            ListView {
                Layout.fillWidth: true
                Layout.preferredHeight: Math.min(contentHeight, Kirigami.Units.gridUnit * 16)
                visible: page.entriesModel && page.entriesModel.subscriptionId === page.inspectId
                model: page.entriesModel
                clip: true

                delegate: Controls.Label {
                    required property string host
                    width: ListView.view ? ListView.view.width : implicitWidth
                    text: host
                }
            }

            Controls.Button {
                Layout.fillWidth: true
                text: "Unsubscribe"
                icon.name: "edit-delete-remove"
                onClicked: {
                    page.model.unsubscribe(page.inspectId);
                    inspector.close();
                }
            }
        }
    }
}
