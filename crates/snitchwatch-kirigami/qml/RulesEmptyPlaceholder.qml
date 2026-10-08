// The Rules page's empty list: no rules, or no list from the firewall
// service yet (PR #106). The page places it on its list, not beside it: a
// ScrollablePage hides every child but its list (`scrollingArea.visible =
// false`), so a placeholder there never showed.
import QtQuick
import org.kde.kirigami as Kirigami

Kirigami.PlaceholderMessage {
    objectName: "rulesEmptyPlaceholder"
    // Whether the bridge has the firewall service's rule list.
    property bool listed: false

    anchors.centerIn: parent
    width: parent ? parent.width - (Kirigami.Units.largeSpacing * 4) : implicitWidth
    icon.name: "view-list-details"
    text: listed ? "No rules yet" : "Waiting for the firewall service's rules"
    explanation: listed
                 ? "Decisions set to “This time” resolve only the current request. Choose a persistent duration, or add a blocklist, to create rules shown here."
                 : "They are listed once the firewall service is connected and has sent them."
}
