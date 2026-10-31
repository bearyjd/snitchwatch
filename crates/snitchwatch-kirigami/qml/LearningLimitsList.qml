// What learning mode cannot promise (the plan's "Honest limits"). Fixed text
// only, as plain facts: nothing here judges a program. Each line is its own
// PlainText label so a test can count them.
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: limits
    spacing: Kirigami.Units.smallSpacing

    readonly property var lines: [
        "While learning runs, every new connection is allowed once, including anything running on this computer in that time that you did not mean to run. Blocklists and deny rules still apply.",
        "A program that runs other programs or scripts (an interpreter or a shell) that is allowed to connect to any destination lets everything run through it connect anywhere. Such a program is never ticked by \"Select suggested\", and ticking \"any destination\" yourself needs an extra confirmation.",
        "Connections are recorded only while Snitchwatch is open. Connections made while it is closed, after a bridge restart (the in-memory sets are lost), or during very heavy load were denied by Snitchwatch's default configuration and are not in this list.",
        "A path does not show what a program is. \"Select suggested\" ticks only \"only these destinations\" rules, for programs that run directly from a system folder, have no flag and are not on the list of programs that run other programs.",
        "The record has limits. When the group of programs with flags is full, new ones are refused, and every other group drops its own oldest entries. What was dropped is counted, but a flood of connections can still push a program you use out.",
        "A folder your user can write to is kept as one entry that lists at most 8 programs; any beyond that are only counted. A path that was cut short or held odd characters cannot be used for a rule, and its row says so.",
        "An \"only these destinations\" rule leaves a shared host (a cloud or CDN address) open to any program later run from the same path, and to one compromised during the learning window. Each row lists its hosts so you can untick it.",
        "The rules list and the rule editor call a folder rule \"all apps\". That wording is a known limit."
    ]

    Repeater {
        model: limits.lines
        delegate: SelectableText {
            required property string modelData
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            text: modelData
        }
    }
}
