// The one list of rule durations every surface offers (decision sheet, alert,
// Make a rule sheet, settings page), and the user's default among them.
//
// Tokens are `pending_decision::parse_duration`'s vocabulary and
// `settings::DURATION_TOKENS` (a test keeps the three equal). Each maps to a
// distinct daemon duration: Once -> "once", 5 minutes -> "5m", Until Restart
// -> "until restart", Forever -> "always". A real "until the program quits"
// is not offered: opensnitchd has no per-process rule lifetime, and the old
// "Until quit" label promised one.
//
// `defaultToken` is the Settings default ("forever" unless changed). main.qml
// binds it to `SettingsController.defaultDuration`; it is read by the alert
// and the sheet to preselect, and by the inline Allow to send.
pragma Singleton
import QtQuick

QtObject {
    readonly property var all: [
        { label: "Once", token: "this_time" },
        { label: "5 minutes", token: "for_5_minutes" },
        { label: "Until Restart", token: "until_restart" },
        { label: "Forever", token: "forever" }
    ]
    // Everything but the first: what Make a rule offers (a once-only answer
    // can't be given afterwards).
    readonly property var remembered: all.slice(1)
    // Replaced by main.qml; this is the same default Rust uses.
    property string defaultToken: "forever"

    function indexOf(token) {
        for (let i = 0; i < all.length; i++) {
            if (all[i].token === token) {
                return i;
            }
        }
        return -1;
    }
}
