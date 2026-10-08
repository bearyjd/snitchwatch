// Blocklists tab (Task 9 — view layer over BlocklistsModel/BlocklistEntriesModel).
//
// Same list/detail shape as ConnectionsPage.qml (Task 8): a ListView of
// subscriptions bound to `BlocklistsModel`, with a Kirigami.OverlaySheet detail
// view (reusing the OverlaySheet inspector pattern established there) showing
// the per-subscription entry (host) list bound to `BlocklistEntriesModel`.
//
// Subscribe/unsubscribe are plain qinvokables on `BlocklistsModel`
// (`subscribe(url)` / `unsubscribe(id)`); they emit `subscriptionRequested`
// with a JSON-encoded `ClientMessage` for the live bridge feed to forward — no
// bridge changes, and consuming that signal into the bridge's live request
// path is the same kind of consumer-side follow-up already noted for
// `ConnectionsModel`'s live wiring.
//
// Status display (last-updated, fetch-failed) reads the bridge's `FetchStatus`
// as already projected into `BlocklistsModel`'s `status` / `statusLabel` /
// `lastUpdated` / `lastFailureReason` roles by the Rust row store — no
// fetch-status logic lives in QML. `status` is only the download result;
// whether a list blocks anything is the separate `enforcementLabel` role
// (issue #45), which never says more than "Rule installed": the firewall
// service accepted the list's rule, and may still have loaded 0 hosts.
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
    property string inspectStatusLabel: ""
    property string inspectEnforcementLabel: ""
    property string inspectEnforcementReason: ""
    property string inspectLastUpdated: ""
    property string inspectLastFailureReason: ""

    // Where the bridge keeps subscriptions (`SetBlocklists.storage`). Not
    // persistent until the bridge says so: no model, no message yet, or an
    // older bridge all mean "kept in memory only".
    readonly property bool storagePersistent: page.model ? page.model.storagePersistent : false
    readonly property string storageReason: page.model ? page.model.storageReason : ""
    // Some list isn't "Rule installed" (issue #45); each row says why. The
    // definite cases (a per-user service, the total size limit) and an
    // unreadable store get their own plain warnings.
    readonly property bool anyNotEnforced: page.model ? page.model.anyNotEnforced : false
    readonly property bool perUserBlocklists: page.model ? page.model.perUserBlocklists : false
    readonly property bool anyOverLimit: page.model ? page.model.anyOverLimit : false
    readonly property bool storageUnreadable: page.model ? page.model.storageUnreadable : false
    // Blocklist rules Snitchwatch made that this service isn't managing
    // (issue #73: no state directory, a per-user service, an unreadable
    // store). They keep blocking; the Rules page won't touch them, so this
    // page offers to remove them.
    readonly property int leftoverRules: page.model ? page.model.leftoverRules : 0
    // Why nothing manages them, and how the last removal went (bridge text,
    // shown in PlainText labels only).
    readonly property string leftoverCause: page.model ? page.model.leftoverCause : ""
    readonly property string leftoverReason: page.model ? page.model.leftoverReason : ""
    // With an unreadable store the rules are probably lists the user still
    // subscribes to: the page says so, and what removing them does.
    readonly property bool leftoversProbablyWanted: page.leftoverCause === "store_unreadable"
    function leftoverNoun() {
        return page.leftoverRules === 1 ? "1 blocklist rule" : page.leftoverRules + " blocklist rules";
    }
    property bool confirmingLeftover: false

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

    // Issue #45: each list becomes a firewall rule that blocks its hosts for
    // every app, but only a row reading "Rule installed" is one the firewall
    // service accepted (or already held). Fixed-text warnings, none
    // dismissable (no close button, no actions): an unreadable store; a
    // per-user service, which applies no blocklists; lists over the total
    // size limit; any other list not confirmed (each row's details say why);
    // and subscriptions kept in memory only, lost on restart. The storage
    // problem's reason is data, so it goes in a PlainText label, never in an
    // InlineMessage (issue #51).
    header: ColumnLayout {
        spacing: 0

        Kirigami.InlineMessage {
            objectName: "unreadableStoreBanner"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: page.storageUnreadable
            text: "Snitchwatch couldn't read its saved blocklists, so it isn't changing any "
                + "blocklist rules the firewall already has."
        }
        // Issue #73. The count is data, so it sits in a PlainText label.
        ColumnLayout {
            objectName: "leftoverNotice"
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            visible: page.leftoverRules > 0
            spacing: Kirigami.Units.smallSpacing

            Controls.Label {
                objectName: "leftoverText"
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Kirigami.Theme.neutralTextColor
                text: page.leftoversProbablyWanted
                    ? page.leftoverNoun() + " made by Snitchwatch "
                      + (page.leftoverRules === 1 ? "is" : "are") + " in the firewall, and "
                      + (page.leftoverRules === 1 ? "is" : "are") + " probably "
                      + (page.leftoverRules === 1 ? "a list" : "lists") + " you still subscribe to: "
                      + "Snitchwatch can't read its saved blocklists, so it isn't changing "
                      + (page.leftoverRules === 1 ? "it" : "them") + ". It checks them only when "
                      + "it starts. Removing " + (page.leftoverRules === 1 ? "it" : "them")
                      + " turns that blocking off; it comes back only if the saved blocklists "
                      + "can be read after a restart, and if they are damaged it stays off."
                    : (page.leftoverRules === 1
                       ? "1 blocklist rule made by Snitchwatch is still in the firewall"
                       : page.leftoverRules + " blocklist rules made by Snitchwatch are still in "
                         + "the firewall")
                      + ", but this service isn't managing " + (page.leftoverRules === 1 ? "it" : "them")
                      + ", so " + (page.leftoverRules === 1 ? "it keeps" : "they keep")
                      + " blocking the hosts of lists you may no longer have."
            }
            Controls.Button {
                objectName: "removeLeftovers"
                visible: !page.confirmingLeftover
                text: "Remove these rules"
                icon.name: "edit-delete-remove"
                onClicked: page.confirmingLeftover = true
            }
            // A removal that failed, or left some behind.
            Controls.Label {
                objectName: "leftoverReasonText"
                Layout.fillWidth: true
                visible: page.leftoverReason.length > 0
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Kirigami.Theme.negativeTextColor
                text: page.leftoverReason
            }
            RowLayout {
                visible: page.confirmingLeftover
                spacing: Kirigami.Units.largeSpacing
                Controls.Label {
                    objectName: "leftoverConfirmText"
                    Layout.fillWidth: true
                    wrapMode: Text.Wrap
                    text: page.leftoversProbablyWanted
                        ? "Remove them? The hosts they block will no longer be blocked until "
                          + "Snitchwatch is restarted with readable saved blocklists, and not at "
                          + "all if they are damaged."
                        : "Remove them from the firewall? The hosts they blocked will no longer "
                          + "be blocked."
                }
                Controls.Button {
                    objectName: "cancelRemoveLeftovers"
                    text: "Cancel"
                    onClicked: page.confirmingLeftover = false
                }
                Controls.Button {
                    objectName: "confirmRemoveLeftovers"
                    text: "Confirm remove"
                    icon.name: "edit-delete-remove"
                    onClicked: {
                        page.model.removeLeftoverRules();
                        page.confirmingLeftover = false;
                    }
                }
            }
        }
        Kirigami.InlineMessage {
            objectName: "perUserBanner"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: page.perUserBlocklists
            text: "This Snitchwatch service runs for your user only, so it doesn't apply blocklists "
                + "to the firewall. That needs the system-wide Snitchwatch service. Blocklist "
                + "rules a system-wide Snitchwatch service left in the firewall can't be checked "
                + "or removed from here; start the system-wide service to remove them."
        }
        Kirigami.InlineMessage {
            objectName: "overLimitBanner"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: page.anyOverLimit
            text: "Some blocklists are over the total size limit of 2,000,000 hosts, so they "
                + "aren't applied to the firewall. Remove a list to make room."
        }
        Kirigami.InlineMessage {
            objectName: "notEnforcedBanner"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: page.anyNotEnforced
            text: "Some blocklists aren't confirmed to be blocking. Open a list to see why."
        }
        Kirigami.InlineMessage {
            objectName: "memoryOnlyStorageBanner"
            Layout.fillWidth: true
            type: Kirigami.MessageType.Warning
            visible: !page.storagePersistent
            text: "Blocklist subscriptions are kept in memory only, so they can't be applied to "
                + "the firewall and are lost when Snitchwatch's background service restarts (for "
                + "example on logout or reboot)."
        }
        Controls.Label {
            objectName: "exactMatchNote"
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            visible: page.model && page.model.count > 0
            wrapMode: Text.Wrap
            opacity: 0.7
            text: "Hosts are matched by exact name, not subdomains."
        }
        Controls.Label {
            Layout.fillWidth: true
            Layout.margins: Kirigami.Units.smallSpacing
            visible: page.storageReason.length > 0
            textFormat: Text.PlainText
            wrapMode: Text.Wrap
            text: "Details: " + page.storageReason
        }
    }

    Kirigami.PlaceholderMessage {
        anchors.centerIn: parent
        width: parent.width - (Kirigami.Units.largeSpacing * 4)
        visible: !page.model || page.model.count === 0
        icon.name: "edit-delete"
        text: "No blocklist subscriptions yet"
        explanation: "Subscribe to a blocklist URL above to block its hosts for every app. "
            + "Hosts are matched by exact name, not subdomains."
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
            required property string statusLabel
            required property string enforcementLabel
            required property string enforcementReason
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
                        textFormat: Text.PlainText
                        text: row.displayName
                        font.bold: true
                        elide: Text.ElideRight
                        Layout.fillWidth: true
                    }
                    Controls.Label {
                        textFormat: Text.PlainText
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
                    textFormat: Text.PlainText
                    text: row.statusLabel
                    color: page.statusColor(row.status)
                    Layout.alignment: Qt.AlignVCenter
                }

                Controls.Label {
                    textFormat: Text.PlainText
                    text: row.enforcementLabel
                    opacity: 0.7
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
        page.inspectStatusLabel = row.statusLabel;
        page.inspectEnforcementLabel = row.enforcementLabel;
        page.inspectEnforcementReason = row.enforcementReason;
        page.inspectLastUpdated = row.lastUpdated;
        page.inspectLastFailureReason = row.lastFailureReason;
        // Entries are never pushed (a whole list in one message overflowed
        // GUI clients, issue #45): ask for the first page. Pages are
        // broadcast to every GUI, so the entries model first notes which
        // list this inspector wants and ignores the rest.
        if (page.entriesModel) {
            page.entriesModel.expectEntries(row.listId);
        }
        if (page.model) {
            page.model.requestEntries(row.listId, 0);
        }
        inspector.open();
    }

    // The list was downloaded again between two pages (issue #67): the model
    // dropped the hosts it held rather than mix old and new ones, so ask for
    // the list from its start.
    Connections {
        target: page.entriesModel
        function onRestartRequested(id) {
            if (page.model && id === page.inspectId) {
                page.model.requestEntries(id, 0);
            }
        }
    }

    // Subscription detail + entries. Kept as an OverlaySheet, same as
    // ConnectionsPage's inspector, so it behaves identically at every width.
    SizedOverlaySheet {
        id: inspector
        title: page.inspectDisplayName

        ColumnLayout {
            Layout.preferredWidth: inspector.preferredWidth
            spacing: Kirigami.Units.largeSpacing

            Kirigami.FormLayout {
                Layout.fillWidth: true
                Controls.Label {
                    Kirigami.FormData.label: "URL"
                    textFormat: Text.PlainText
                    text: page.inspectUrl
                    elide: Text.ElideMiddle
                }
                Controls.Label {
                    Kirigami.FormData.label: "Entries"
                    text: page.inspectEntryCount
                }
                Controls.Label {
                    Kirigami.FormData.label: "Download"
                    textFormat: Text.PlainText
                    text: page.inspectStatusLabel
                    color: page.statusColor(page.inspectStatus)
                }
                Controls.Label {
                    Kirigami.FormData.label: "Blocking"
                    textFormat: Text.PlainText
                    text: page.inspectEnforcementLabel
                }
                Controls.Label {
                    Kirigami.FormData.label: "Why"
                    visible: page.inspectEnforcementReason.length > 0
                    textFormat: Text.PlainText
                    text: page.inspectEnforcementReason
                    wrapMode: Text.Wrap
                }
                Controls.Label {
                    Kirigami.FormData.label: "Last updated"
                    visible: page.inspectLastUpdated.length > 0
                    textFormat: Text.PlainText
                    text: page.inspectLastUpdated
                }
                Controls.Label {
                    Kirigami.FormData.label: "Last failure"
                    visible: (page.inspectStatus === "failed" || page.inspectStatus === "refused")
                        && page.inspectLastFailureReason.length > 0
                    textFormat: Text.PlainText
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
            // time, loaded a page at a time on request (openInspector, "Show
            // more"). Show them when they match what's being inspected.
            Kirigami.PlaceholderMessage {
                Layout.fillWidth: true
                visible: !page.entriesModel || page.entriesModel.subscriptionId !== page.inspectId
                icon.name: "view-refresh"
                text: "Entries not loaded"
                explanation: "Waiting for Snitchwatch's background service to send this list's hosts."
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
                    textFormat: Text.PlainText
                    text: host
                }
            }

            Controls.Label {
                visible: page.entriesModel && page.entriesModel.subscriptionId === page.inspectId
                text: page.entriesModel
                    ? "Showing " + page.entriesModel.count + " of " + page.entriesModel.total + " hosts"
                    : ""
                opacity: 0.7
            }

            Controls.Button {
                Layout.fillWidth: true
                visible: page.entriesModel && page.entriesModel.subscriptionId === page.inspectId
                    && page.entriesModel.hasMore
                text: "Show more"
                icon.name: "go-down"
                onClicked: page.model.requestEntries(page.inspectId, page.entriesModel.count)
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
