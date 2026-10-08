// Pending-decision surface (Task 7; Parity 2 — durations, insight).
//
// The safety-critical allow/deny controls for a novel connection. Embedded in
// the Connections inspector for a pending row; reusable as a standalone content
// block. The two verdict buttons (allow/deny) plus the host-match scope and
// rule-duration selectors map — in Rust (`pending_decision.rs`), not here —
// onto the bridge's typed `ClientMessage::SetVerdict`. This QML only collects
// the choice.
//
// Timeout ownership: `remainingSeconds` is *displayed* only; the auto-action
// countdown is owned server-side by the bridge's AskRule machinery. This
// component never runs its own timer.
//
// The insight panel (Parity 2) is a strictly decorative side-channel: a lookup
// failure/timeout NEVER disables or delays the Allow/Deny buttons below. See
// `insight_model.rs` and `connections::row_store::row_by_id` for where its
// data comes from.
//
// Deliberately no per-connection traffic readout or sparkline (issue #49): the
// bridge hardcodes every connection's byte counters to 0 and opensnitchd
// v1.8.0 has no per-connection counters, so anything drawn from them would be
// a fabricated "0 B" / flat line. Restore it only once a real byte source
// exists.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import com.snitchwatch.shell

ColumnLayout {
    id: sheet
    spacing: Kirigami.Units.largeSpacing

    // Populated by the caller from the selected pending row.
    property string rowId: ""
    property string process: ""
    property string host: ""
    // Remote IP (Parity 2 insight panel target), from
    // `ConnectionsModel.rowDetailsJson`.
    property string remoteIp: ""
    // Server-owned countdown to the automatic fallback action. Negative hides it.
    property int remainingSeconds: -1

    // Live-wiring hub (Task 13), injected from ConnectionsPage. When set, a
    // submitted verdict's JSON is routed to the bridge's inbound pump. Null in
    // isolated component tests, in which case the verdict is a no-op sink.
    property var bridgeFeed: null

    // Emitted after a verdict is submitted so the container can close/advance.
    signal decided()

    // Reverse-DNS + RDAP lookup surface (Parity 2). Qt-free fetch/cache logic
    // lives in `insight::client`; this QObject only dispatches it async and
    // never blocks `lookup()`'s caller.
    PendingInsight {
        id: insight
    }

    // Kick off the best-effort insight lookup whenever the target IP changes
    // (a fresh pending row, or the sheet initially populating). A no-op for
    // an empty IP.
    onRemoteIpChanged: insight.lookup(sheet.remoteIp)
    Component.onCompleted: insight.lookup(sheet.remoteIp)

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        visible: true
        type: Kirigami.MessageType.Warning
        text: sheet.process + " wants to connect to " + sheet.host
    }

    RowLayout {
        Layout.fillWidth: true
        Controls.Label {
            text: "Scope"
            Layout.alignment: Qt.AlignVCenter
        }
        Controls.ComboBox {
            id: scopeBox
            Layout.fillWidth: true
            textRole: "label"
            valueRole: "token"
            // Back-reference: `model[0]`'s token ("this_host") is the
            // default `ConnectionsPage.qml`'s inline Allow/Deny buttons
            // hardcode (`submitInlineVerdict`) so an inline decision matches
            // what this sheet would submit unchanged. Reordering this model
            // or changing its first entry's token changes that default too.
            model: [
                { label: "This host only", token: "this_host" },
                { label: "Any host on this domain", token: "any_host_on_domain" },
                { label: "Any host", token: "any_host" }
            ]
        }
    }

    // Granular rule scopes (Parity 2): how long the resulting rule should
    // live. Maps onto the bridge's `VerdictDuration` — see
    // `pending_decision.rs`'s doc comment for the full duration-mapping
    // table, including the one lossy case ("Until quit" -> daemon
    // "until restart").
    RowLayout {
        Layout.fillWidth: true
        Controls.Label {
            text: "Duration"
            Layout.alignment: Qt.AlignVCenter
        }
        Controls.ComboBox {
            id: durationBox
            Layout.fillWidth: true
            textRole: "label"
            valueRole: "token"
            // Back-reference: `model[0]`'s token ("this_time") is the
            // default `ConnectionsPage.qml`'s inline Allow/Deny buttons
            // hardcode (`submitInlineVerdict`), same rationale as
            // `scopeBox`'s model comment above.
            model: [
                { label: "This time", token: "this_time" },
                { label: "For 5 minutes", token: "for_5_minutes" },
                { label: "Until quit", token: "until_quit" },
                { label: "Forever", token: "forever" }
            ]
        }
    }

    // Countdown display only — never a client-side timer.
    Controls.Label {
        Layout.fillWidth: true
        horizontalAlignment: Text.AlignHCenter
        opacity: 0.7
        visible: sheet.remainingSeconds >= 0
        text: "Auto-action in " + sheet.remainingSeconds + "s"
    }

    // Insight panel (Parity 2) — best-effort research on the remote host.
    // Never gates the verdict buttons below: a hung/offline lookup shows
    // "Looking up..."/"unavailable (offline?)" forever, nothing more.
    Kirigami.FormLayout {
        Layout.fillWidth: true
        visible: sheet.remoteIp.length > 0

        Controls.Label {
            Kirigami.FormData.label: "Reverse DNS"
            text: insight.loading
                  ? "Looking up…"
                  : (insight.hostname.length > 0
                     ? insight.hostname
                     : "No PTR result for " + sheet.remoteIp)
        }
        Controls.Label {
            Kirigami.FormData.label: "Organization"
            visible: insight.org.length > 0
            text: insight.org
        }
        Controls.Label {
            Kirigami.FormData.label: "Registrar"
            visible: insight.registrar.length > 0
            text: insight.registrar
        }
        Controls.Label {
            Kirigami.FormData.label: "Country"
            visible: insight.country.length > 0
            text: insight.country
        }
        Controls.Label {
            Kirigami.FormData.label: "Registration info"
            visible: !insight.loading && !insight.available && insight.rdapEnabled
            opacity: 0.7
            text: "unavailable (offline?)"
        }
        Controls.Label {
            Kirigami.FormData.label: "Registration info"
            visible: !insight.rdapEnabled
            opacity: 0.7
            text: "Online research disabled — enable in Settings"
        }
    }

    RowLayout {
        Layout.fillWidth: true
        spacing: Kirigami.Units.smallSpacing

        Controls.Button {
            Layout.fillWidth: true
            text: "Allow"
            icon.name: "dialog-ok-apply"
            onClicked: sheet.submit("allow")
        }
        Controls.Button {
            Layout.fillWidth: true
            text: "Deny"
            icon.name: "edit-delete-remove"
            onClicked: sheet.submit("deny")
        }
    }

    function submit(action) {
        if (sheet.bridgeFeed !== null) {
            sheet.bridgeFeed.submitVerdict(
                sheet.rowId, action, scopeBox.currentValue, durationBox.currentValue);
        } else {
            // Unreachable in the running app; logged rather than dropped
            // silently so a mis-wired container can't lose a decision without
            // a trace. `decided()` still fires so the sheet closes either way.
            console.warn("PendingDecisionSheet: no bridgeFeed; verdict dropped for", sheet.rowId);
        }
        sheet.decided();
    }
}
