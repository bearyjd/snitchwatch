// The one list of silent modes (onboarding step and settings page).
//
// Tokens are `settings::SilentMode`'s (`off`, `allow`, `deny`; a test keeps
// them equal). "Allow and record" / "Deny and record" answer each new
// connection once-only at arrival and mark it "needs a rule"
// (`silent_mode.rs`); neither stores a rule by itself.
pragma Singleton
import QtQuick

QtObject {
    readonly property var all: [
        { label: "Ask me (alerts)", token: "off",
          detail: "A small alert asks about each new connection, even when the main window is closed to the tray." },
        { label: "Allow and record", token: "allow",
          detail: "New connections are allowed once and listed as needing a rule." },
        { label: "Deny and record", token: "deny",
          detail: "New connections are denied once and listed as needing a rule." }
    ]
    // Preselected in onboarding.
    readonly property string recommended: "allow"
    // Honest about what silent mode cannot do (shown wherever it is chosen).
    readonly property string notRunningNote:
        "When Snitchwatch is not running at all, the firewall's own default action applies, whatever you choose here."
    readonly property string denyWarning:
        "Deny and record blocks DNS lookups and NetworkManager's connectivity checks until rules for them exist, so the network may seem broken at first."

    function indexOf(token) {
        for (let i = 0; i < all.length; i++) {
            if (all[i].token === token) {
                return i;
            }
        }
        return -1;
    }
}
